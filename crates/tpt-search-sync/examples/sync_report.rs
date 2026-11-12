//! Prints a sync report for a small offline-then-reconnect scenario.
//!
//! Run with: `cargo run -p tpt-search-sync --example sync_report`

use tpt_search_core::DocumentId;
use tpt_search_sync::{BandwidthManager, SyncEngine, SyncMonitor};

fn main() {
    let bw = BandwidthManager::default();
    let mut monitor = SyncMonitor::new();
    let mut cloud = SyncEngine::new(0);
    let mut phone = SyncEngine::new(1);
    let mut laptop = SyncEngine::new(2);
    let note = DocumentId::from("note-1");

    phone
        .set_field(&note, "title", "draft from phone".into())
        .unwrap();
    monitor.record(
        "phone upload",
        phone.upload(&mut cloud, &bw, usize::MAX).unwrap(),
    );
    monitor.record("laptop download", laptop.download(&cloud, &bw).unwrap());

    // Both devices go offline and edit the same field.
    phone
        .set_field(&note, "title", "phone edit".into())
        .unwrap();
    laptop
        .set_field(&note, "title", "laptop edit".into())
        .unwrap();
    laptop.set_field(&note, "tags", "travel".into()).unwrap();

    monitor.record(
        "phone upload",
        phone.upload(&mut cloud, &bw, usize::MAX).unwrap(),
    );
    monitor.record(
        "laptop upload",
        laptop.upload(&mut cloud, &bw, usize::MAX).unwrap(),
    );
    monitor.record("phone download", phone.download(&cloud, &bw).unwrap());
    print!("{}", monitor.render());
}
