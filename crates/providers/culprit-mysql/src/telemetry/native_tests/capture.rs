//! Existing provider factory and sealed snapshot receipt reader shared by owned native controls.
use super::replication::required;
use super::*;
use culprit_collector::{
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::Outbox,
};
use std::path::Path;

pub(in crate::telemetry::native_tests) struct Factory(pub(in crate::telemetry::native_tests) crate::MysqlProvider);

#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.0.telemetry_source(request)
    }
}

pub(in crate::telemetry::native_tests) struct Capture {
    pub manifest: CollectionManifest,
    pub records: Vec<ExportedRecord>,
}

pub(in crate::telemetry::native_tests) async fn original(
    provider: crate::MysqlProvider,
    request: CollectionRequest,
    binding: &str,
) -> Box<Capture> {
    let temporary = tempfile::tempdir().unwrap();
    let mut outbox = Box::new(Outbox::create(&temporary.path().join("outbox"), OutboxLimits::default()).unwrap());
    let mut lease = Box::new(Box::pin(outbox.reserve(binding, &request)).await.unwrap());
    let output = lease.directory().to_path_buf();
    let identity = lease.identity().clone();
    let factory = Factory(provider);
    let result = Box::new(
        Box::pin(capture::collect(
            &culprit_read::admission::SourceGate::default(),
            &factory,
            request.clone(),
            &output,
            identity.clone(),
            &CollectionCancellation::default(),
            Some(lease.as_mut()),
        ))
        .await,
    );
    let finished = Box::pin((*lease).finish()).await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = required(*result, "actual native source/Outbox capture");
    required(finished, "sealed native original completion");
    let sealed = required(Box::pin(outbox.verify(&identity.observation_id)).await, "verified sealed native original");
    assert_eq!(sealed.capture, manifest.capture);
    Box::new(read_capture(sealed, &request.budget, &output))
}

pub(in crate::telemetry::native_tests) fn read_capture(
    manifest: CollectionManifest,
    budget: &CollectionBudget,
    output: &Path,
) -> Capture {
    assert!(manifest.source_queries <= budget.max_queries);
    let TelemetryCollection::Snapshot(dataset) = manifest.request.collection() else {
        panic!("original native snapshot request");
    };
    let mut records = Vec::new();
    for file in &manifest.files {
        assert_eq!(file.stream, dataset.stream());
        let bytes = std::fs::read(output.join(&file.file)).expect("native capture file");
        assert_eq!(file.bytes as usize, bytes.len());
        if let Some(hash) = &file.sha256 {
            assert_eq!(hash, &digest(&bytes), "sealed native file receipt");
        }
        let exported = std::str::from_utf8(&bytes)
            .expect("native NDJSON")
            .lines()
            .map(|line| serde_json::from_str::<ExportedRecord>(line).expect("typed native exported record"))
            .collect::<Vec<_>>();
        assert_eq!(file.rows as usize, exported.len());
        for row in exported {
            assert_eq!(row.capture, manifest.capture);
            assert_eq!(row.batch_id, file.batch_id);
            assert!(row.record.is_valid());
            if manifest.status == CollectionStatus::Complete {
                let source = manifest.description.as_ref().expect("completed native source");
                let canonical = required(
                    ExportedRecord::new(source, &manifest.capture, row.record.clone()),
                    "canonical native export",
                );
                assert_eq!(serde_json::to_value(&row).unwrap(), serde_json::to_value(canonical).unwrap());
            }
            records.push(row);
        }
    }
    assert!(
        manifest.read_metrics.as_ref().is_some_and(|metrics| metrics.is_valid() && !metrics.has_unconfirmed_work())
    );
    Capture { manifest, records }
}
