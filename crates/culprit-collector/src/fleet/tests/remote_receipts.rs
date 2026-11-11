use super::*;
use crate::delivery::{
    contract::Contract,
    evidence::{BatchManifest, PreparedRange, RangeManifest},
    file_store::FileRecoveryStore,
    recovery::stage_and_commit,
};
use std::sync::Arc;

fn backfill_job(inventory: &FleetInventory) -> Job {
    let mut scheduler = Scheduler::new(inventory, "one").unwrap();
    scheduler.take(Duration::ZERO).unwrap().unwrap();
    scheduler.finish(Duration::ZERO, scheduler::Outcome::Complete).unwrap();
    scheduler.take(Duration::from_secs(60)).unwrap().unwrap()
}
fn prepared_interval(root: &std::path::Path, backend: &Backend, contract: Contract) -> PreparedRange {
    let capture = CaptureIdentity {
        observation_id: "remote-history".into(),
        revision: Some(CaptureRevision {
            outbox_id: "test-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let source = CollectionDescription {
        database: backend.database.lock().unwrap().clone(),
        source: "query_store".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval: interval(10),
            replica_group_ids: vec!["0".into()],
            capture_mode: "ALL".into(),
            waits_capture_enabled: true,
        }),
    };
    let directory = root.join("original-capture");
    std::fs::create_dir(&directory).unwrap();
    let batches = source
        .observation
        .collection()
        .streams()
        .iter()
        .map(|&stream| {
            std::fs::write(directory.join(stream.file_name()), []).unwrap();
            let sha256 = digest(b"");
            BatchManifest {
                batch_id: batch_id(&capture.observation_id, stream).unwrap(),
                stream,
                object: format!("payloads/{sha256}.ndjson"),
                sha256,
                bytes: 0,
                rows: vec![],
                delivery_required: true,
            }
        })
        .collect();
    let range_id = digest(&serde_json::to_vec(&(&contract, &capture)).unwrap());
    PreparedRange {
        manifest: Arc::new(RangeManifest {
            contract,
            range_id,
            capture,
            source,
            collected_from: "2026-11-10T12:00:00Z".into(),
            collected_until: "2026-11-10T12:00:01Z".into(),
            read_metrics: None,
            batches,
            advances: vec![],
        }),
        directory,
    }
}

#[tokio::test]
async fn remote_receipt_skips_an_old_native_interval_after_local_payload_and_receipt_loss() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 14_400, reconcile_seconds: 60 });
    let job = backfill_job(&inventory);
    assert!(job.lane == scheduler::HistoryLane::Backfill);
    let backend = Backend::new();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let prepared = prepared_interval(root.path(), &backend, contract);
    stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    std::fs::remove_dir_all(&prepared.directory).unwrap();
    let mut outbox = outbox(&root.path().join("new-outbox")).await;
    let mut storage = CaptureStorage { outbox: &mut outbox, recovery: Some(&store) };
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let cancellation = CollectionCancellation::default();
    let now = work::parse_time("2026-11-10T14:00:00Z").unwrap();
    work::execute(&backend, &journal, &mut storage, &job, &cancellation, now).await.unwrap();
    let result = work::execute(&backend, &journal, &mut storage, &job, &cancellation, now).await.unwrap();
    assert_eq!(result.event.work, FleetWork::SkipCommitted);
    assert!(backend.requests.lock().unwrap().is_empty());
    work::execute(&backend, &journal, &mut storage, &job, &cancellation, now).await.unwrap();
    assert_eq!(
        backend.requests.lock().unwrap().len(),
        1,
        "recent intervals still reconcile against their native source"
    );
}

#[tokio::test]
async fn receipt_storage_failure_retains_the_pending_interval_without_falling_back_to_sql() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 14_400, reconcile_seconds: 60 });
    let job = backfill_job(&inventory);
    let backend = Backend::new();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract).unwrap();
    rusqlite::Connection::open(root.path().join("recovery/recovery.sqlite3"))
        .unwrap()
        .execute("DROP TABLE coverage", [])
        .unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let mut storage = CaptureStorage { outbox: &mut outbox, recovery: Some(&store) };
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let cancellation = CollectionCancellation::default();
    let now = work::parse_time("2026-11-10T14:00:00Z").unwrap();
    work::execute(&backend, &journal, &mut storage, &job, &cancellation, now).await.unwrap();
    for _ in 0..2 {
        let result = work::execute(&backend, &journal, &mut storage, &job, &cancellation, now).await.unwrap();
        assert_eq!(result.event.work, FleetWork::SkipCommitted);
        assert!(matches!(result.event.status, CollectionStatus::Incomplete(_)));
    }
    assert!(backend.requests.lock().unwrap().is_empty());
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
}
