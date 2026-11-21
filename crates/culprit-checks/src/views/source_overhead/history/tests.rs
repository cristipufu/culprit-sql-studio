use super::*;
use culprit_core::{CapturedSourceAcquisition, SourceAcquisitionSeal, SourceOverheadCaptureSummary, telemetry::*};
fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json"))).unwrap()
}
fn original() -> Box<CapturedSourceAcquisition> {
    Box::new(
        CapturedSourceAcquisition::captured(
            digest(b"original"),
            SourceAcquisitionSeal::Sealed,
            SourceOverheadCaptureSummary {
                capture: CaptureIdentity {
                    observation_id: "a".repeat(32),
                    revision: Some(CaptureRevision {
                        outbox_id: "b".repeat(32),
                        owner_epoch: "1".into(),
                        sequence: "1".into(),
                    }),
                },
                engine: None,
                status: CollectionStatus::Incomplete(CollectionStop::SourceUnavailable),
                started_at: "2020-01-01T00:00:00Z".into(),
                finished_at: Some("2020-01-01T00:00:01Z".into()),
                source_queries: Some(7),
                read_metrics: Some(ReadWorkSummary {
                    connection_work: None,
                    native_session_work: None,
                    active_tasks: 0,
                    untracked_tasks: 7,
                    templates: vec![],
                }),
            },
        )
        .unwrap(),
    )
}
#[test]
fn failed_route_history_keeps_original_clocks_unknown_identity_and_cost_qualification_after_current_scan_drift() {
    let mut snapshot = snapshot();
    snapshot.identity = None;
    snapshot.connection = "another-current-route".into();
    snapshot.operations.source_acquisitions = vec![RecordedSourceAcquisition::Captured { original: original() }];
    let original = snapshot.operations.source_acquisitions.clone();
    let reports = explain(&snapshot);
    let [HistoricalSourceOverhead::Available { report, .. }] = reports.as_slice() else { panic!() };
    assert_eq!(report.evidence.engine, None);
    assert_eq!(report.evidence.source_queries, Some(7));
    assert_eq!(report.evidence.started_at, "2020-01-01T00:00:00Z");
    assert!(report.limitations.contains(&culprit_core::t!("view.source_overhead.limit.historical")));
    assert!(report.limitations.contains(&culprit_core::t!("view.source_overhead.limit.incomplete")));
    assert!(report.limitations.contains(&culprit_core::t!("view.source_overhead.limit.native")));
    assert!(report.native_interval.is_none());
    assert_eq!(snapshot.operations.source_acquisitions, original);
}
#[test]
fn tampered_or_provisional_saved_metadata_cannot_become_available_or_borrow_a_peer_original() {
    for cause in ["counter", "provisional", "fingerprint"] {
        let mut first = original();
        match cause {
            "counter" => first.summary.source_queries = Some(8),
            "fingerprint" => first.fingerprint = digest(b"bad"),
            _ => first.seal = SourceAcquisitionSeal::Provisional,
        }
        let mut snapshot = snapshot();
        snapshot.operations.source_acquisitions = vec![RecordedSourceAcquisition::Captured { original: first }];
        let reports = explain(&snapshot);
        assert!(
            matches!(reports.as_slice(),[HistoricalSourceOverhead::Unavailable{reason}]
            if reason==&culprit_core::t!("view.source_overhead.invalid_saved")),
            "{cause}"
        );
    }
}
#[test]
fn conflicts_after_a_presentation_prefix_withdraw_all_matching_copies_and_overflow_is_explicit() {
    let first = original();
    let mut changed = first.summary.clone();
    changed.source_queries = Some(9);
    let second =
        Box::new(CapturedSourceAcquisition::captured(first.manifest_sha256.clone(), first.seal, changed).unwrap());
    let mut snapshot = snapshot();
    snapshot.operations.source_acquisitions.push(RecordedSourceAcquisition::Captured { original: first });
    snapshot.operations.source_acquisitions.extend(std::iter::repeat_n(
        RecordedSourceAcquisition::Unavailable { reason: message!("view.source_overhead.initial_missing") },
        MAX_RECORDED_SOURCE_ACQUISITIONS + 1,
    ));
    snapshot.operations.source_acquisitions.push(RecordedSourceAcquisition::Captured { original: second });
    let before = serde_json::to_value(&snapshot.operations.source_acquisitions).unwrap();
    let reports = explain(&snapshot);
    assert_eq!(reports.len(), MAX_RECORDED_SOURCE_ACQUISITIONS + 1);
    assert!(reports.iter().all(|report| matches!(report, HistoricalSourceOverhead::Unavailable { .. })));
    assert_eq!(serde_json::to_value(&snapshot.operations.source_acquisitions).unwrap(), before);
    let HistoricalSourceOverhead::Unavailable { reason } = reports.last().unwrap() else { panic!() };
    assert_eq!(reason, &message!("view.source_overhead.history_overflow", shown = "32", recorded = "34").render());
}
#[test]
fn sharing_withdraws_each_indivisible_input_and_reanalysis_keeps_no_hash_body_or_native_measurement() {
    let mut snapshot = snapshot();
    let original = original();
    let digest = original.manifest_sha256.clone();
    snapshot.operations.source_acquisitions = vec![RecordedSourceAcquisition::Captured { original }];
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(matches!(
        shared.operations.source_acquisitions.as_slice(),
        [RecordedSourceAcquisition::Unavailable { .. }]
    ));
    let report = explain(&shared);
    assert!(matches!(report.as_slice(),[HistoricalSourceOverhead::Unavailable{reason}]
        if reason==&culprit_core::t!("view.source_overhead.sharing_withheld")));
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(&digest));
    assert!(!encoded.contains("2020-01-01"));
    assert_eq!(
        culprit_core::redact::redact_snapshot(&shared).operations.source_acquisitions,
        shared.operations.source_acquisitions
    );
}
#[test]
fn empty_history_is_empty_rather_than_a_claim_of_zero_current_scan_work() {
    let mut snapshot = snapshot();
    snapshot.operations.source_acquisitions.clear();
    assert!(explain(&snapshot).is_empty());
}
