//! Consistency under leader changes: every acknowledged write must be visible, through
//! a linearizable read, on every surviving node, immediately after re-election.

use std::time::Duration;

use tpt_search_core::{Document, DocumentId, FieldSchema, FieldType, Schema};
use tpt_search_distributed::Cluster;

fn schema() -> Schema {
    Schema::new("docs", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(FieldSchema::new("n", FieldType::Integer))
        .unwrap()
}

fn doc(i: usize) -> Document {
    Document::new(format!("doc-{i}")).with_field("n", i as i64)
}

/// Waits until a leader other than `old` is elected.
async fn new_leader_besides(cluster: &Cluster, old: u64) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(l) = cluster.leader() {
            if l != old {
                return l;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no new leader after {old} stopped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn assert_all_visible(cluster: &Cluster, ids: &[usize]) {
    for node in cluster.online() {
        for &i in ids {
            let got = cluster
                .get(node, &DocumentId::from(format!("doc-{i}")))
                .await
                .unwrap();
            assert!(got.is_some(), "node {node} is missing doc-{i}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acknowledged_writes_are_visible_on_every_survivor_after_failover() {
    let cluster = Cluster::start(schema(), [1, 2, 3]).await.unwrap();
    cluster.initialize().await.unwrap();

    let first: Vec<usize> = (0..30).collect();
    for &i in &first {
        cluster.upsert(doc(i)).await.unwrap();
    }
    let leader = cluster.leader().unwrap();
    cluster.stop(leader).await.unwrap();
    // Read straight away, before anything else happens: the failover must not lose data.
    assert_all_visible(&cluster, &first).await;

    let second: Vec<usize> = (30..60).collect();
    for &i in &second {
        cluster.upsert(doc(i)).await.unwrap();
    }
    let all: Vec<usize> = (0..60).collect();
    assert_all_visible(&cluster, &all).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_survives_two_successive_leader_failures_in_five_nodes() {
    let cluster = Cluster::start(schema(), [1, 2, 3, 4, 5]).await.unwrap();
    cluster.initialize().await.unwrap();

    let mut written = Vec::new();
    for i in 0..10 {
        cluster.upsert(doc(i)).await.unwrap();
        written.push(i);
    }
    let first_leader = cluster.leader().unwrap();
    cluster.stop(first_leader).await.unwrap();
    new_leader_besides(&cluster, first_leader).await;

    for i in 10..20 {
        cluster.upsert(doc(i)).await.unwrap();
        written.push(i);
    }
    let second_leader = cluster.leader().unwrap();
    cluster.stop(second_leader).await.unwrap();
    new_leader_besides(&cluster, second_leader).await;

    for i in 20..25 {
        cluster.upsert(doc(i)).await.unwrap();
        written.push(i);
    }
    assert_all_visible(&cluster, &written).await;
    assert!(cluster.status().has_quorum);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn concurrent_writers_keep_every_acknowledged_write_across_a_leader_kill() {
    use std::sync::Arc;

    let cluster = Arc::new(Cluster::start(schema(), [1, 2, 3]).await.unwrap());
    cluster.initialize().await.unwrap();

    // Four writers, each with its own id range. Record only writes that were acknowledged.
    let mut writers = Vec::new();
    for w in 0..4usize {
        let cluster = Arc::clone(&cluster);
        writers.push(tokio::spawn(async move {
            let mut acked = Vec::new();
            for k in 0..25usize {
                let i = w * 1000 + k;
                if cluster.upsert(doc(i)).await.is_ok() {
                    acked.push(i);
                }
            }
            acked
        }));
    }

    // Kill the leader while the writers are in flight.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let leader = cluster.leader().unwrap();
    cluster.stop(leader).await.unwrap();

    let mut acked = Vec::new();
    for w in writers {
        acked.extend(w.await.unwrap());
    }
    assert!(!acked.is_empty(), "some writes must succeed");
    assert_all_visible(&cluster, &acked).await;
}
