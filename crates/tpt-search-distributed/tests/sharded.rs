//! Sharded replicated search: placement, routing, cross-shard search, and failure
//! isolation between shard groups.

use tpt_search_core::{Analyzer, Document, DocumentId, FieldSchema, FieldType, Schema};
use tpt_search_distributed::{ShardedCluster, ShardingStrategy};
use tpt_search_index::TermQuery;

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
}

fn doc(i: usize) -> Document {
    let title = if i % 2 == 0 {
        "rust search"
    } else {
        "vector database"
    };
    Document::new(format!("paper-{i}")).with_field("title", title)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn documents_route_to_shards_and_search_spans_all_shards() {
    let cluster = ShardedCluster::start(schema(), ShardingStrategy::Hash { shards: 3 }, 3, 1..=9)
        .await
        .unwrap();

    for i in 0..30 {
        cluster.upsert(doc(i)).await.unwrap();
    }
    // Each document lives on exactly one shard.
    let mut per_shard = [0usize; 3];
    for i in 0..30 {
        let id = DocumentId::from(format!("paper-{i}"));
        let owner = cluster.manager().route_document(id.as_str());
        per_shard[owner as usize] += 1;
        assert!(
            cluster.get(&id).await.unwrap().is_some(),
            "paper-{i} missing"
        );
    }
    assert_eq!(per_shard.iter().sum::<usize>(), 30);
    assert!(
        per_shard.iter().all(|&n| n > 0),
        "every shard should hold documents"
    );

    let hits = cluster
        .search(&TermQuery {
            field: "title".into(),
            text: "rust".into(),
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 15, "every even-numbered paper matches");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_failure_in_one_shard_leaves_the_others_serving() {
    let cluster = ShardedCluster::start(schema(), ShardingStrategy::Hash { shards: 3 }, 3, 1..=9)
        .await
        .unwrap();
    for i in 0..12 {
        cluster.upsert(doc(i)).await.unwrap();
    }

    // Kill the leader of shard 0 and keep writing to every shard.
    let shard0 = cluster.shard(0).unwrap();
    let leader = shard0.leader().unwrap();
    shard0.stop(leader).await.unwrap();

    for i in 12..24 {
        cluster.upsert(doc(i)).await.unwrap();
    }
    for i in 0..24 {
        let id = DocumentId::from(format!("paper-{i}"));
        assert!(
            cluster.get(&id).await.unwrap().is_some(),
            "paper-{i} missing after failover"
        );
    }
    let hits = cluster
        .search(&TermQuery {
            field: "title".into(),
            text: "rust".into(),
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 12);
}
