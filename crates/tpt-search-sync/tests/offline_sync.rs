//! Edge-to-cloud sync: offline edits queue durably, survive a restart, reconnect, and
//! converge with conflicting edits resolved automatically.

use std::path::PathBuf;

use tpt_search_core::{DocumentId, FieldValue};
use tpt_search_sync::{BandwidthManager, SyncEngine, SyncMonitor};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tpt-sync-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn id(s: &str) -> DocumentId {
    DocumentId::from(s)
}

#[test]
fn offline_edits_queue_survive_restart_and_converge_with_conflicts() {
    let dir = scratch("cycle");
    let bw = BandwidthManager::default();
    let mut monitor = SyncMonitor::new();
    let mut cloud = SyncEngine::new(0);

    // Phase 1: online. Edge A creates a note and syncs it up; edge B pulls it down.
    let mut a = SyncEngine::open(1, &dir.join("a.wal")).unwrap();
    let mut b = SyncEngine::open(2, &dir.join("b.wal")).unwrap();
    a.set_field(&id("note-1"), "title", "v0".into()).unwrap();
    let up = a.upload(&mut cloud, &bw, usize::MAX).unwrap();
    monitor.record("a upload (online)", up);
    let down = b.download(&cloud, &bw).unwrap();
    monitor.record("b download (online)", down);
    assert_eq!(
        b.get(&id("note-1"), "title"),
        Some(&FieldValue::Text("v0".into()))
    );

    // Phase 2: both edges go offline and edit the same field, plus separate fields.
    a.set_field(&id("note-1"), "title", "from a".into())
        .unwrap();
    a.set_field(&id("note-1"), "body", "a wrote this".into())
        .unwrap();
    b.set_field(&id("note-1"), "title", "from b".into())
        .unwrap();
    b.set_field(&id("note-1"), "tags", "offline".into())
        .unwrap();
    assert_eq!(a.pending().len(), 2);
    assert_eq!(b.pending().len(), 2);

    // Edge A restarts while offline. Its queue must come back from the log.
    let a_pending_before = a.pending();
    drop(a);
    let mut a = SyncEngine::open(1, &dir.join("a.wal")).unwrap();
    assert_eq!(a.pending(), a_pending_before, "queue survives a restart");
    assert_eq!(
        a.get(&id("note-1"), "title"),
        Some(&FieldValue::Text("from a".into()))
    );

    // Phase 3: reconnect. Both edges upload their queues. Conflicts resolve by LWW.
    let a_up = a.upload(&mut cloud, &bw, usize::MAX).unwrap();
    monitor.record("a upload (reconnect)", a_up);
    assert!(
        a.pending().is_empty(),
        "acknowledged entries leave the queue"
    );
    let b_up = b.upload(&mut cloud, &bw, usize::MAX).unwrap();
    monitor.record("b upload (reconnect)", b_up.clone());
    assert_eq!(
        b_up.conflicts, 1,
        "one field was written independently on both edges"
    );

    // Phase 4: everyone converges. Both edges pull the cloud's state.
    monitor.record("a download", a.download(&cloud, &bw).unwrap());
    monitor.record("b download", b.download(&cloud, &bw).unwrap());
    let title_cloud = cloud.get(&id("note-1"), "title").cloned();
    assert_eq!(a.get(&id("note-1"), "title").cloned(), title_cloud);
    assert_eq!(b.get(&id("note-1"), "title").cloned(), title_cloud);
    assert_eq!(a.get(&id("note-1"), "tags"), b.get(&id("note-1"), "tags"));
    assert_eq!(
        a.get(&id("note-1"), "body"),
        Some(&FieldValue::Text("a wrote this".into()))
    );
    assert_eq!(
        b.get(&id("note-1"), "tags"),
        Some(&FieldValue::Text("offline".into()))
    );

    let report = monitor.render();
    assert!(report.contains("a upload (reconnect)"));
    assert!(report.contains("total"));
    // Observed once by the cloud (on b's upload), once by A and once by B (on download): the
    // two conflicting writes meet on three replicas.
    assert_eq!(monitor.totals().conflicts, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bandwidth_budget_defers_then_delivers_everything() {
    let bw = BandwidthManager::new(0).with_priority_fields(["title"]);
    let mut cloud = SyncEngine::new(0);
    // Edge 7 has many body edits. Edge 8 has one priority-field edit.
    let mut bulk = SyncEngine::new(7);
    let mut urgent = SyncEngine::new(8);
    for i in 0..20 {
        bulk.set_field(
            &id(&format!("doc-{i}")),
            "body",
            format!("content {i} ").repeat(8).into(),
        )
        .unwrap();
    }
    urgent
        .set_field(&id("doc-0"), "title", "priority".into())
        .unwrap();

    // The urgent replica's single entry fits and arrives at once.
    let urgent_up = urgent.upload(&mut cloud, &bw, 600).unwrap();
    assert_eq!(urgent_up.deferred, 0);
    assert_eq!(
        cloud.get(&id("doc-0"), "title"),
        Some(&FieldValue::Text("priority".into()))
    );

    // A small window sends only part of the bulk queue.
    let first = bulk.upload(&mut cloud, &bw, 600).unwrap();
    assert!(first.deferred > 0, "some entries must wait");
    assert!(first.bytes_sent > 0);
    assert!(!bulk.pending().is_empty());

    // Keep uploading until the queue drains.
    let mut rounds = 1;
    while !bulk.pending().is_empty() {
        bulk.upload(&mut cloud, &bw, 600).unwrap();
        rounds += 1;
        assert!(rounds < 100, "queue never drained");
    }
    for i in 0..20 {
        assert!(
            cloud.get(&id(&format!("doc-{i}")), "body").is_some(),
            "doc-{i} did not arrive"
        );
    }
}

#[test]
fn deletes_propagate_and_later_edits_revive_documents() {
    let bw = BandwidthManager::default();
    let mut cloud = SyncEngine::new(0);
    let mut phone = SyncEngine::new(1);
    let mut laptop = SyncEngine::new(2);

    phone.set_field(&id("doc"), "title", "x".into()).unwrap();
    phone.upload(&mut cloud, &bw, usize::MAX).unwrap();
    laptop.download(&cloud, &bw).unwrap();

    laptop.delete(&id("doc")).unwrap();
    laptop.upload(&mut cloud, &bw, usize::MAX).unwrap();
    phone.download(&cloud, &bw).unwrap();
    assert!(phone.live_documents().is_empty(), "delete propagated");

    phone.set_field(&id("doc"), "title", "back".into()).unwrap();
    phone.upload(&mut cloud, &bw, usize::MAX).unwrap();
    laptop.download(&cloud, &bw).unwrap();
    assert_eq!(
        laptop.live_documents(),
        [id("doc")],
        "a newer edit revives the document"
    );
}
