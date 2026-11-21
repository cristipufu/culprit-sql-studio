use super::*;
use crate::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, OutboxCaptureReader,
};
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, query_wait_fragment_evidence},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{SourceState, limits::OperationGuard, telemetry::*};
async fn original() -> Box<CapturedQueryHistoryRead> {
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
        Some(RecordedQueryHistoryRead::Unavailable { .. })
    ));
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap());
    let report = replay.query_wait_fragments.unwrap();
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert!(report.read.is_none() && report.description.is_none() && report.entries.is_empty());
    let twice = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(twice.operations.query_wait_fragments, shared.operations.query_wait_fragments);
}

async fn runtime_original() -> Box<CapturedQueryHistoryRead> {
    let (source, rows) = culprit_collector::analytical::test_support::query_runtime_fragment_evidence(true);
    let fixture = LocalCaptureFixture::query_history(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let value = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::RuntimeFragments,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::RuntimeFragments(value) = value else { panic!() };
    value.into_read()
}
#[tokio::test]
async fn saved_runtime_and_wait_originals_coexist_with_independent_capture_clocks_and_share_withdrawal() {
    let runtime = runtime_original().await;
    let waits = original().await;
    assert_ne!(runtime.read.capture, waits.read.capture);
    let runtime_source = runtime.description.clone();
    let wait_source = waits.description.clone();
    let runtime_bounds = (runtime.collected_from.clone(), runtime.collected_until.clone());
    let wait_bounds = (waits.collected_from.clone(), waits.collected_until.clone());
    let mut snapshot = snapshot();
    snapshot.identity = None;
    CapturedScanEvidence::runtime_fragments(runtime.clone()).unwrap().attach(&mut snapshot).unwrap();
    CapturedScanEvidence::query_wait_fragments(waits.clone()).unwrap().attach(&mut snapshot).unwrap();
    assert!(CapturedScanEvidence::runtime_fragments(waits).is_err());
    assert!(CapturedScanEvidence::query_wait_fragments(runtime.clone()).is_err());
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap());
    let native = replay.runtime_fragments.unwrap();
    let native_wait = replay.query_wait_fragments.unwrap();
    assert_eq!(native.description.as_deref(), Some(&runtime_source));
    assert_eq!(native_wait.description.as_deref(), Some(&wait_source));
    assert_eq!((native.collected_from, native.collected_until), (Some(runtime_bounds.0), Some(runtime_bounds.1)));
    assert_eq!((native_wait.collected_from, native_wait.collected_until), (Some(wait_bounds.0), Some(wait_bounds.1)));
    assert_eq!(native.entries.len(), 2);
    assert!(!native_wait.entries.is_empty());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let shared = culprit_checks::analyze(shared);
    for (state, empty, original) in [
        (
            shared.runtime_fragments.as_ref().unwrap().source_state,
            shared.runtime_fragments.as_ref().unwrap().entries.is_empty(),
            shared.runtime_fragments.as_ref().unwrap().read.is_none(),
        ),
        (
            shared.query_wait_fragments.as_ref().unwrap().source_state,
            shared.query_wait_fragments.as_ref().unwrap().entries.is_empty(),
            shared.query_wait_fragments.as_ref().unwrap().read.is_none(),
        ),
    ] {
        assert_eq!(state, SourceState::Unavailable);
        assert!(empty && original);
    }
}
