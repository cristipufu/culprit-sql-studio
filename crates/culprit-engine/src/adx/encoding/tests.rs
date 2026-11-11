use super::*;
use crate::adx::{
    indexes::ProjectionIndexes,
    ready::ReadyDispatcher,
    test_support::{Blobs, Fault, Recovery},
};
use culprit_collector::delivery::{recovery::reopen, sink::DeliveryAttempt};
fn scratch_files(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with(TEMP_PREFIX))
        .count()
}
#[tokio::test]
async fn only_complete_verified_recovery_can_create_indexes_and_ready_objects_and_redrive_emits_fresh_names() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery = Arc::new(Recovery::new(&catalog));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let blobs = Arc::new(Blobs::default());
    let indexes = Arc::new(ProjectionIndexes::new(blobs.clone()));
    let encoder = BatchEncoder::new(catalog, recovery.clone(), temp.path()).unwrap();
    let dispatcher = ReadyDispatcher::new(encoder, indexes, blobs.clone());
    let attempt = DeliveryAttempt::new(1).unwrap();
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        *recovery.fault.lock().unwrap() = fault;
        assert!(dispatcher.dispatch(&range, &attempt, &range.manifest().batches[0].batch_id).await.is_err());
        assert!(blobs.objects.lock().unwrap().is_empty());
        assert!(blobs.uploads.lock().unwrap().is_empty());
        assert_eq!(scratch_files(temp.path()), 0);
    }
    *recovery.fault.lock().unwrap() = Fault::Stall;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            dispatcher.dispatch(&range, &attempt, &range.manifest().batches[0].batch_id)
        )
        .await
        .is_err()
    );
    assert!(blobs.objects.lock().unwrap().is_empty());
    assert_eq!(scratch_files(temp.path()), 0);
    *recovery.fault.lock().unwrap() = Fault::None;
    dispatcher.dispatch(&range, &attempt, &range.manifest().batches[0].batch_id).await.unwrap();
    dispatcher.dispatch(&range, &attempt, &range.manifest().batches[0].batch_id).await.unwrap();
    dispatcher
        .dispatch(&range, &DeliveryAttempt::new(2).unwrap(), &range.manifest().batches[0].batch_id)
        .await
        .unwrap();
    assert_eq!(scratch_files(temp.path()), 0);
    {
        let objects = blobs.objects.lock().unwrap();
        let ready = objects.iter().filter(|(key, _)| key.starts_with("ready/")).collect::<Vec<_>>();
        assert_eq!(ready.len(), 2);
        assert!(ready[0].0.starts_with("ready/CulpritResourceSamplesV1/"));
        let row: serde_json::Value = serde_json::from_slice(ready[0].1).unwrap();
        assert_eq!(row["RangeId"], range.manifest().range_id);
        assert_eq!(row["RowOrdinal"], 1);
        assert_eq!(objects.len(), 3); // one immutable expected index, two independently routable attempts
    }
    let key = blobs.uploads.lock().unwrap()[0].clone();
    blobs.objects.lock().unwrap().get_mut(&key).unwrap()[0] ^= 1;
    assert!(dispatcher.dispatch(&range, &attempt, &range.manifest().batches[0].batch_id).await.is_err());
}
#[tokio::test]
async fn scratch_ownership_survives_encoder_drop_until_the_encoded_batch_is_released_and_restart_cleans_abandoned_files()
 {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery = Arc::new(Recovery::new(&catalog));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    std::fs::write(temp.path().join(format!("{TEMP_PREFIX}abandoned")), b"abandoned").unwrap();
    let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), temp.path()).unwrap();
    assert_eq!(scratch_files(temp.path()), 0);
    let batch = encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap();
    assert_eq!(scratch_files(temp.path()), 1);
    drop(encoder);
    let lock_path = temp.path().join("projection-owner.lock");
    let other = std::fs::OpenOptions::new().read(true).write(true).open(lock_path).unwrap();
    assert!(matches!(other.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    drop(batch);
    assert!(other.try_lock().is_ok());
    other.unlock().unwrap();
    assert_eq!(scratch_files(temp.path()), 0);
    BatchEncoder::new(catalog, recovery, temp.path()).unwrap();
}
