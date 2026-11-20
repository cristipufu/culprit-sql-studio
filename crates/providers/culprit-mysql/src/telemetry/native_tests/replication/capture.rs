use super::*;
use crate::telemetry::native_tests::capture::{Capture, Factory, read_capture};
use culprit_collector::capture::{CollectionCancellation, collect_once};
use culprit_read::admission::SourceGate;
use std::path::Path;

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
