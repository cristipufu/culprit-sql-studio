use super::*;
use culprit_collector::capture::{CollectionCancellation, SourceFactory, collect_once};
use culprit_read::admission::SourceGate;
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

pub(in crate::telemetry::native_tests) fn request(budget: CollectionBudget) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication },
        budget,
    }
}

pub(super) async fn collect(fixture: &Fixture, login: Login, budget: CollectionBudget) -> Capture {
    let directory = tempfile::tempdir().expect("private native conformance artifacts");
    let output = directory.path().join("replication");
    let manifest = required(attempt(fixture, login, &budget, &output).await, "replication collection");
    read_capture(manifest, &budget, &output)
}

pub(super) async fn denied(fixture: &Fixture) -> Capture {
    let directory = tempfile::tempdir().expect("private denied native conformance artifacts");
    let output = directory.path().join("replication");
    let budget = budget();
    let result = attempt(fixture, Login::Denied, &budget, &output).await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("denied SHOW must return its original permission failure"),
    };
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    // The controller commits its failed attempt before returning the original native permission error.
    // Reopen that immutable artifact; never replace the failed API result with a fabricated empty capture.
    let manifest: CollectionManifest = serde_json::from_slice(
        &std::fs::read(output.join("manifest.json")).expect("controller's denied capture artifact"),
    )
    .expect("original denied native manifest");
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable));
    assert!(
        manifest.description.as_ref().is_none_or(|source| source.observation.snapshot().is_none()),
        "a failed initial native read cannot certify an empty or successful snapshot"
    );
    read_capture(manifest, &budget, &output)
}

async fn attempt(
    fixture: &Fixture,
    login: Login,
    budget: &CollectionBudget,
    output: &Path,
) -> Result<CollectionManifest> {
    let factory = Factory(fixture.provider(login));
    let result = collect_once(
        &SourceGate::default(),
        &factory,
        request(budget.clone()),
        output,
        &CollectionCancellation::default(),
    )
    .await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent, "native source must settle before publication");
    result
}

pub(in crate::telemetry::native_tests) fn read_capture(
    manifest: CollectionManifest,
    budget: &CollectionBudget,
    output: &Path,
) -> Capture {
    assert!(manifest.source_queries <= budget.max_queries);
    let mut records = Vec::new();
    for file in &manifest.files {
        assert_eq!(file.stream, TelemetryStream::ReplicationSamples);
        let bytes = std::fs::read(output.join(&file.file)).expect("native replication capture file");
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

pub(in crate::telemetry::native_tests) fn show_work<'a>(
    metrics: &'a ReadWorkMetrics,
    flavor: Flavor,
    version: &str,
) -> &'a ReadQueryMetrics {
    let query = native::replication::query(flavor, version);
    let templates = metrics.templates.iter().filter(|metric| matches!(&metric.statement,
        DefinitionPayload::Available { text, .. } if ["SHOW REPLICA STATUS", "SHOW SLAVE STATUS", "SHOW ALL REPLICAS STATUS", "SHOW ALL SLAVES STATUS"]
            .iter().any(|query| text.contains(query)))).collect::<Vec<_>>();
    assert_eq!(templates.len(), 1, "one replication SHOW template; no alias retry or fallback");
    let metric = templates[0];
    assert!(matches!(&metric.statement, DefinitionPayload::Available { text, .. } if text.contains(query)));
    assert_eq!(metric.completed_tasks, 1);
    metric
}

pub(in crate::telemetry::native_tests) fn samples(rows: &[ExportedRecord]) -> Vec<&NativeReplicationSample> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Replication(point) = &row.record else { panic!("native replication stream") };
            assert_eq!(point.source, native::replication::SOURCE);
            let ReplicationFact::Native(sample) = &point.fact else { panic!("native SHOW channel") };
            sample.as_ref()
        })
        .collect()
}
