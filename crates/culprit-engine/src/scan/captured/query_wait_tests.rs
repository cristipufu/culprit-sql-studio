use super::*;
use crate::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, OutboxCaptureReader,
};
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, query_wait_fragment_evidence},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{SourceState, limits::OperationGuard, telemetry::*};
async fn original() -> Box<CapturedQueryWaitRead> {
    let (source, rows) = query_wait_fragment_evidence();
    let fixture = LocalCaptureFixture::query_history(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::QueryWaitFragments,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::QueryWaitFragments(value) = result else { panic!() };
    value.into_read()
}
fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json"))).unwrap()
}
#[tokio::test]
async fn saved_wait_original_replays_with_original_database_interval_and_fragments_after_route_drift() {
    let read = original().await;
    let expected = read.description.clone();
    let mut snapshot = snapshot();
    snapshot.identity = None;
    CapturedScanEvidence::query_wait_fragments(read.clone()).unwrap().attach(&mut snapshot).unwrap();
    CapturedScanEvidence::query_wait_fragments(read).unwrap().attach(&mut snapshot).unwrap();
    let saved: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let report = culprit_checks::analyze(saved);
    let waits = report.query_wait_fragments.unwrap();
    assert_eq!(waits.description.as_deref(), Some(&expected));
    assert_eq!(waits.entries.len(), 2);
    assert_eq!(
        waits.description.unwrap().query_history().unwrap().interval.native_id,
        expected.query_history().unwrap().interval.native_id
    );
    assert!(
        report
            .snapshot
            .workload
            .as_ref()
            .unwrap()
            .queries
            .iter()
            .all(|query| query.waits.iter().all(|wait| wait.wait_type.as_deref() != Some("native-page-io")))
    );
}
#[tokio::test]
async fn conflicting_selected_originals_refuse_and_sharing_withdraws_native_history_as_one_indivisible_input() {
    let read = original().await;
    let mut snapshot = snapshot();
    CapturedScanEvidence::query_wait_fragments(read.clone()).unwrap().attach(&mut snapshot).unwrap();
    let second = original().await;
    assert_ne!(read.read.capture, second.read.capture);
    assert!(CapturedScanEvidence::query_wait_fragments(second).unwrap().attach(&mut snapshot).is_err());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(matches!(
        shared.operations.query_wait_fragments.as_deref(),
        Some(RecordedQueryWaitRead::Unavailable { .. })
    ));
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap());
    let report = replay.query_wait_fragments.unwrap();
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert!(report.read.is_none() && report.description.is_none() && report.entries.is_empty());
    let twice = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(twice.operations.query_wait_fragments, shared.operations.query_wait_fragments);
}
