//! Saved originals come from the real local Outbox boundary; the later scan never supplies native authority.
use super::*;
use crate::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, LocalSnapshotAnalysis,
    OutboxCaptureReader,
};
use culprit_collector::{
    analytical::test_support::{CdcCase, ChangeFeedCase, LocalCaptureFixture, cdc_evidence, change_feed_evidence},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{SourceState, limits::OperationGuard, telemetry::*};
async fn original(family: MssqlChangeCaptureFamily) -> LocalSnapshotAnalysis {
    let (source, rows) = match family {
        MssqlChangeCaptureFamily::Cdc => cdc_evidence(CdcCase::ErrorAndProgress),
        MssqlChangeCaptureFamily::ChangeFeed => change_feed_evidence(ChangeFeedCase::PartialErrors),
    };
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let kind = match family {
        MssqlChangeCaptureFamily::Cdc => LocalCaptureAnalysisKind::Cdc,
        MssqlChangeCaptureFamily::ChangeFeed => LocalCaptureAnalysisKind::ChangeFeed,
    };
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: kind,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
    result
}
fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json"))).unwrap()
}
#[tokio::test]
async fn independent_original_families_replay_idempotently_after_current_database_and_member_drift() {
    let cdc = original(MssqlChangeCaptureFamily::Cdc).await.into_read();
    let feed = original(MssqlChangeCaptureFamily::ChangeFeed).await.into_read();
    let mut saved = snapshot();
    saved.identity = None;
    saved.server.database = "later-unrelated-database".into();
    for read in [cdc.clone(), cdc.clone()] {
        CapturedScanEvidence::change_capture(MssqlChangeCaptureFamily::Cdc, read).unwrap().attach(&mut saved).unwrap();
    }
    CapturedScanEvidence::change_capture(MssqlChangeCaptureFamily::ChangeFeed, feed.clone())
        .unwrap()
        .attach(&mut saved)
        .unwrap();
    assert_eq!(saved.operations.cdc_history.len(), 1);
    assert_eq!(saved.operations.change_feed_history.len(), 1);
    let saved: Snapshot = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
    let report = culprit_checks::analyze(saved);
    let cdc_report = &report.cdc.as_ref().unwrap()[0];
    let feed_report = &report.change_feed.as_ref().unwrap()[0];
    assert_eq!(cdc_report.context.as_ref().unwrap().target, cdc.description.target);
    assert_eq!(feed_report.context.as_ref().unwrap().partition, feed.description.partition);
    assert_eq!(
        cdc_report.context.as_ref().unwrap().observed_until,
        cdc.description.observation.snapshot().unwrap().observed_until
    );
    assert_eq!(feed_report.source_state, SourceState::Partial);
    assert_eq!(feed_report.sources[2].records, 2);
    let displayed = serde_json::to_value(crate::reports::display(&report)).unwrap();
    assert_eq!(displayed["cdc"], serde_json::to_value(&report.cdc).unwrap());
    assert_eq!(displayed["change_feed"], serde_json::to_value(&report.change_feed).unwrap());
    assert_eq!(report.snapshot.operations.cdc_history[0], RecordedOperationalRead::Captured { read: cdc });
    assert_eq!(report.snapshot.operations.change_feed_history[0], RecordedOperationalRead::Captured { read: feed });
}
#[tokio::test]
async fn foreign_family_and_changed_original_receipts_never_keep_a_measured_survivor() {
    let read = original(MssqlChangeCaptureFamily::Cdc).await.into_read();
    assert!(CapturedScanEvidence::change_capture(MssqlChangeCaptureFamily::ChangeFeed, read.clone()).is_err());
    let mut saved = snapshot();
    CapturedScanEvidence::change_capture(MssqlChangeCaptureFamily::Cdc, read.clone())
        .unwrap()
        .attach(&mut saved)
        .unwrap();
    let mut changed = read.clone();
    changed.read_complete = false;
    assert!(
        CapturedScanEvidence::change_capture(MssqlChangeCaptureFamily::Cdc, changed.clone())
            .unwrap()
            .attach(&mut saved)
            .is_err()
    );
    saved.operations.cdc_history.push(RecordedOperationalRead::Captured { read: changed });
    let raw = saved.operations.cdc_history.clone();
    let report = culprit_checks::analyze(saved);
    assert_eq!(report.snapshot.operations.cdc_history, raw);
    let cdc = &report.cdc.unwrap()[0];
    assert_eq!(cdc.source_state, SourceState::Unavailable);
    assert!(cdc.context.is_none() && cdc.entries.is_empty());
}
#[tokio::test]
async fn sharing_withdraws_both_independent_histories_instead_of_masking_or_reissuing_original_receipts() {
    let mut saved = snapshot();
    for family in [MssqlChangeCaptureFamily::Cdc, MssqlChangeCaptureFamily::ChangeFeed] {
        CapturedScanEvidence::change_capture(family, original(family).await.into_read())
            .unwrap()
            .attach(&mut saved)
            .unwrap();
    }
    let shared = culprit_core::redact::redact_snapshot(&saved);
    let twice = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(shared.operations.cdc_history, twice.operations.cdc_history);
    assert_eq!(shared.operations.change_feed_history, twice.operations.change_feed_history);
    for originals in [&shared.operations.cdc_history, &shared.operations.change_feed_history] {
        assert!(matches!(&originals[0], RecordedOperationalRead::Unavailable { .. }));
        assert!(!serde_json::to_string(originals).unwrap().contains("original-instance"));
    }
    let report = culprit_checks::analyze(shared);
    let cdc = &report.cdc.unwrap()[0];
    let feed = &report.change_feed.unwrap()[0];
    assert!(cdc.context.is_none() && feed.context.is_none());
    assert!(cdc.entries.is_empty() && feed.entries.is_empty());
    assert!(!cdc.read_complete && !feed.read_complete);
}
