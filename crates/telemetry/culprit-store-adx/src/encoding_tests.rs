use crate::{
    catalog::SchemaCatalog,
    test_support::{Blobs, Fault, Queries, Recovery},
};
use culprit_collector::analytical::encoding::{BatchEncoder, TEMP_PREFIX};
use culprit_collector::delivery::{
    recovery::reopen,
    sink::{DeliveryAttempt, TelemetrySink},
};
use std::{path::Path, sync::Arc};

struct HeldUpload {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    finished: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl culprit_collector::delivery::blob_store::transport::BlobTransport for HeldUpload {
    async fn get(
        &self,
        _: &str,
    ) -> culprit_core::Result<Option<culprit_collector::delivery::blob_store::transport::BlobRead>> {
        unreachable!()
    }
    async fn put(
        &self,
        _: &str,
        _: Vec<u8>,
        _: culprit_collector::delivery::blob_store::transport::WriteCondition<'_>,
    ) -> culprit_core::Result<culprit_collector::delivery::blob_store::transport::BlobWrite> {
        unreachable!()
    }
    async fn put_file(
        &self,
        _: &str,
        path: &Path,
        bytes: u64,
    ) -> culprit_core::Result<culprit_collector::delivery::blob_store::transport::BlobWrite> {
        self.started.notify_one();
        self.release.notified().await;
        assert_eq!(std::fs::read(path).unwrap().len() as u64, bytes);
        self.finished.notify_one();
        Ok(culprit_collector::delivery::blob_store::transport::BlobWrite::Written)
    }
}

#[tokio::test]
async fn cancelling_ready_dispatch_retains_the_projection_until_the_transport_finishes() {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
    let batch = Arc::new(encoder.encode(&range, &range.manifest().batches[0].batch_id).await.unwrap());
    let retained = Arc::downgrade(&batch);
    drop(encoder);
    let upload = Arc::new(HeldUpload {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        finished: tokio::sync::Notify::new(),
    });
    let dispatcher = crate::ready::ReadyDispatcher::new(upload.clone());
    let task = tokio::spawn(async move { dispatcher.dispatch(&range, &DeliveryAttempt::new(1).unwrap(), batch).await });
    upload.started.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(retained.upgrade().is_some());
    assert!(BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).is_err());
    upload.release.notify_one();
    upload.finished.notified().await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while retained.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    culprit_collector::analytical::test_support::released_spool(catalog, recovery, scratch.path()).await;
}
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
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let blobs = Arc::new(Blobs::default());
    let dispatcher = crate::sink::new(
        range.manifest().contract.clone(),
        recovery.clone(),
        blobs.clone(),
        Arc::new(Queries::default()),
        temp.path(),
    )
    .unwrap();
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
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
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

#[tokio::test]
async fn provider_neutral_query_contracts_encode_through_verified_recovery_without_sql_server_fields() {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    for recovery in [
        crate::test_support::interval_queries("adx:test", catalog.digest()),
        crate::test_support::cumulative_queries("adx:test", catalog.digest()),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let recovery = Arc::new(recovery);
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let blobs = Arc::new(Blobs::default());
        let dispatcher = crate::sink::new(
            range.manifest().contract.clone(),
            recovery.clone(),
            blobs.clone(),
            Arc::new(Queries::default()),
            temp.path(),
        )
        .unwrap();
        for batch in &range.manifest().batches {
            dispatcher.dispatch(&range, &DeliveryAttempt::new(1).unwrap(), &batch.batch_id).await.unwrap();
        }
        let objects = blobs.objects.lock().unwrap();
        for (key, bytes) in objects.iter().filter(|(key, _)| key.starts_with("ready/")) {
            for line in std::str::from_utf8(bytes).unwrap().lines() {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                if key.contains("CulpritQueryRuntimeV1") {
                    let metrics: serde_json::Value = serde_json::from_str(row["Metrics"].as_str().unwrap()).unwrap();
                    assert_eq!(row["Executions"], "9007199254740993");
                    assert_eq!(metrics[1]["unit"]["bytes_per_page"], 16384);
                    assert_eq!(row["HasPlanDefinitionId"], false);
                }
                if key.contains("CulpritQueryWaitsV1") {
                    assert_eq!(row["TimeUnit"], "microseconds");
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(row["TotalWait"].as_str().unwrap()).unwrap()["value"],
                        1.25
                    );
                }
                if key.contains("CulpritOperationalV1") {
                    let details: serde_json::Value = serde_json::from_str(row["Details"].as_str().unwrap()).unwrap();
                    assert_eq!(row["RangeKind"], "current");
                    assert_eq!(details["totals"][0]["total"]["value"], "9007199254740993");
                    assert_eq!(details["reset"]["kind"], "unknown");
                }
            }
        }
    }
}
