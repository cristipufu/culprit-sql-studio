use super::*;
use crate::delivery::{
    driver::{DeliveryDriver, DeliveryPolicy},
    file_sink::FileTelemetrySink,
    file_store::FileRecoveryStore,
    recovery::stage_and_commit,
    tests::{contract, prepared, problem},
};
use culprit_core::telemetry::DeliveryPhase;
use std::sync::Arc;

#[tokio::test]
async fn a_fresh_delivery_journal_publishes_remote_ranges_after_all_original_local_state_is_lost() {
    let remote = tempfile::TempDir::new().unwrap();
    let original = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FileRecoveryStore::create(&remote.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let first = prepared(original.path(), 1);
    let id = first.manifest().range_id.clone();
    stage_and_commit(&*store, owner.fence(), None, &first).await.unwrap();
    let second = prepared(original.path(), 2);
    stage_and_commit(&*store, owner.fence(), Some(&id), &second).await.unwrap();
    drop(first);
    drop(second);
    drop(original);
    let replacement = tempfile::TempDir::new().unwrap();
    let directory = replacement.path().join("delivery");
    let journal = DeliveryJournal::create(&directory, contract()).unwrap();
    let page = restore_page(&journal, &*store, None, 1).await.unwrap();
    assert_eq!(journal.status(&id).await.unwrap().phase, DeliveryPhase::Committed);
    restore_page(&journal, &*store, None, 1).await.unwrap();
    drop(journal);
    let mut journal = DeliveryJournal::open(&directory, contract()).unwrap();
    let next = restore_page(&journal, &*store, page.next_cursor.as_deref(), 1).await.unwrap();
    assert!(next.next_cursor.is_none());
    assert_eq!(next.high_sequence, 2);
    let sink = FileTelemetrySink::create(&remote.path().join("warehouse"), contract(), store.clone()).unwrap();
    let policy = DeliveryPolicy { visibility_delay_ms: 1, ..Default::default() };
    let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
    let mut driver = DeliveryDriver::new(&mut journal, &*store, &sink, &mut capabilities, policy).unwrap();
    let mut now = 0;
    for _ in 0..10 {
        let status = driver.step(&id, owner.fence(), now).await.unwrap();
        if status.phase == DeliveryPhase::Retired {
            break;
        }
        now = status.retry_at_ms.parse::<i64>().unwrap().max(now + 1);
    }
    assert_eq!(journal.status(&id).await.unwrap().phase, DeliveryPhase::Retired);
    assert_eq!(sink.physical_rows(&id).await.unwrap(), 1);
    restore_page(&journal, &*store, None, 1).await.unwrap();
    assert_eq!(
        journal.status(&id).await.unwrap().phase,
        DeliveryPhase::Retired,
        "restoring cannot reset delivery progress"
    );
}

#[tokio::test]
async fn a_different_recovery_namespace_cannot_adopt_existing_delivery_backlog() {
    let root = tempfile::TempDir::new().unwrap();
    let first = FileRecoveryStore::create(&root.path().join("first"), contract()).unwrap();
    let second = FileRecoveryStore::create(&root.path().join("second"), contract()).unwrap();
    let journal = DeliveryJournal::create(&root.path().join("delivery"), contract()).unwrap();
    restore_page(&journal, &first, None, 1).await.unwrap();
    drop(journal);
    let journal = DeliveryJournal::open(&root.path().join("delivery"), contract()).unwrap();
    assert_eq!(
        problem(restore_page(&journal, &second, None, 1).await.err().unwrap()),
        DeliveryProblem::DestinationChanged
    );
}

#[tokio::test]
async fn cold_recovery_restores_only_unpublished_work_and_keeps_historical_source_receipts() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let first = prepared(root.path(), 1);
    let range = stage_and_commit(&*store, owner.fence(), None, &first).await.unwrap();
    let second = prepared(root.path(), 2);
    stage_and_commit(&*store, owner.fence(), Some(&range.manifest().range_id), &second).await.unwrap();
    let sink = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store.clone()).unwrap();
    use crate::delivery::sink::*;
    assert!(read_publication(&sink, &range).await.is_err());
    let attempt = DeliveryAttempt::new(1).unwrap();
    dispatch_range(&sink, &range, &attempt).await.unwrap();
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    sink.publish(&verified, &attempt).await.unwrap();
    let publication = read_publication(&sink, &range).await.unwrap();
    store.retire(owner.fence(), &publication).await.unwrap();
    drop(owner);
    let store = FileRecoveryStore::open(&root.path().join("recovery"), contract()).unwrap();
    let page = store.pending_ranges(None, 128, RecoveryOrder::OldestFirst).await.unwrap();
    assert_eq!(page.ranges.len(), 1);
    assert_eq!(page.ranges[0], *second.manifest());
    assert!(page.next_cursor.is_none());
    assert_eq!(page.high_sequence, 2);
    assert_eq!(store.recover(None, 128).await.unwrap().ranges.len(), 2);
    assert!(store.captured_range(&range.manifest().source.captured_range().unwrap()).await.unwrap().is_some());
    let journal = DeliveryJournal::create(&root.path().join("replacement"), contract()).unwrap();
    restore_pending_page(&journal, &store, None, 128).await.unwrap();
    assert!(journal.status(&range.manifest().range_id).await.is_err());
    assert_eq!(journal.status(&second.manifest().range_id).await.unwrap().phase, DeliveryPhase::Committed);
}
