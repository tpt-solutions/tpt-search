//! Raft-replicated search cluster: replication, linearizable reads, leader failover,
//! membership changes, and loss of quorum.

use std::time::Duration;

use tpt_search_core::{
    Analyzer, DistributedError, Document, DocumentId, FieldSchema, FieldType, Schema,
};
use tpt_search_distributed::Cluster;
use tpt_search_index::TermQuery;

const WAIT: Duration = Duration::from_secs(10);

fn schema() -> Schema {
    Schema::new("papers", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("title", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::english()),
        )
        .unwrap()
        .with_field(FieldSchema::new("year", FieldType::Integer).indexed())
        .unwrap()
}

fn paper(id: &str, title: &str, year: i64) -> Document {
    Document::new(id)
        .with_field("title", title)
        .with_field("year", year)
}

async fn three_node_cluster() -> Cluster {
    let cluster = Cluster::start(schema(), [1, 2, 3]).await.unwrap();
    cluster.initialize().await.unwrap();
    cluster.wait_for_leader(WAIT).await.unwrap();
    cluster
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_replicate_and_reads_are_consistent_on_every_node() {
    let cluster = three_node_cluster().await;
    cluster
        .upsert(paper("p1", "rust search engine", 2021))
        .await
        .unwrap();
    cluster
        .upsert(paper("p2", "vector search tutorial", 2019))
        .await
        .unwrap();
    cluster
        .upsert(paper("p3", "cooking pasta", 2023))
        .await
        .unwrap();

    for node in [1, 2, 3] {
        let doc = cluster.get(node, &DocumentId::from("p2")).await.unwrap();
        assert_eq!(
            doc.and_then(|d| d.get("year").cloned()),
            Some(2019i64.into()),
            "node {node}"
        );
        let hits = cluster
            .search(
                node,
                &TermQuery {
                    field: "title".into(),
                    text: "search".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 2, "node {node}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletes_replicate() {
    let cluster = three_node_cluster().await;
    cluster
        .upsert(paper("gone", "temporary", 2000))
        .await
        .unwrap();
    assert!(cluster.delete(DocumentId::from("gone")).await.unwrap());
    for node in [1, 2, 3] {
        assert!(cluster
            .get(node, &DocumentId::from("gone"))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_rejections_are_reported_identically_on_every_replica() {
    let cluster = three_node_cluster().await;
    let bad = Document::new("bad").with_field("title", 42i64);
    assert!(matches!(
        cluster.upsert(bad).await,
        Err(DistributedError::Consensus(_))
    ));
    for node in [1, 2, 3] {
        assert!(cluster
            .get(node, &DocumentId::from("bad"))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_failover_keeps_committed_data_and_accepts_new_writes() {
    let cluster = three_node_cluster().await;
    cluster
        .upsert(paper("before", "written under the first leader", 2020))
        .await
        .unwrap();

    let old_leader = cluster.leader().expect("leader");
    cluster.stop(old_leader).await.unwrap();

    // The survivors elect a new leader.
    let new_leader = {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(l) = cluster.leader() {
                if l != old_leader {
                    break l;
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "no new leader");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    assert_ne!(new_leader, old_leader);

    cluster
        .upsert(paper("after", "written under the second leader", 2024))
        .await
        .unwrap();
    for node in cluster.online() {
        assert!(
            cluster
                .get(node, &DocumentId::from("before"))
                .await
                .unwrap()
                .is_some(),
            "node {node} lost data"
        );
        assert!(
            cluster
                .get(node, &DocumentId::from("after"))
                .await
                .unwrap()
                .is_some(),
            "node {node} missing new write"
        );
    }

    let status = cluster.status();
    assert!(status.has_quorum);
    let stopped = status.nodes.iter().find(|n| n.id == old_leader).unwrap();
    assert!(!stopped.online);
    assert_eq!(stopped.state, "Offline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_without_quorum_do_not_commit() {
    let cluster = three_node_cluster().await;
    let leader = cluster.leader().expect("leader");
    let others: Vec<u64> = cluster
        .online()
        .into_iter()
        .filter(|n| *n != leader)
        .collect();
    for node in &others {
        cluster.stop(*node).await.unwrap();
    }
    // With two of three nodes down, the leader cannot commit.
    let attempt = tokio::time::timeout(
        Duration::from_secs(2),
        cluster.upsert(paper("stuck", "cannot commit", 2025)),
    )
    .await;
    assert!(
        attempt.is_err() || attempt.unwrap().is_err(),
        "write committed without quorum"
    );
    let status = cluster.status();
    assert!(!status.has_quorum);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn added_node_catches_up_and_removed_node_leaves() {
    let mut cluster = three_node_cluster().await;
    cluster
        .upsert(paper("seed", "existing data", 2010))
        .await
        .unwrap();

    cluster.add_node(4).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    while cluster
        .get(4, &DocumentId::from("seed"))
        .await
        .unwrap()
        .is_none()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "node 4 did not catch up"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    cluster.remove_node(1).await.unwrap();
    // If node 1 was the leader, the rest of the cluster has to elect a new one.
    cluster.wait_for_leader(WAIT).await.unwrap();
    let online = cluster.online();
    assert!(!online.contains(&1));
    cluster
        .upsert(paper("later", "after membership change", 2026))
        .await
        .unwrap();
    assert!(cluster
        .get(4, &DocumentId::from("later"))
        .await
        .unwrap()
        .is_some());
}
