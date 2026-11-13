use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::statement_work_evidence_cases,
};
use culprit_core::{ErrorDetail, MeasurementScope, ResetMarker, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn observations(rows: &[TelemetryRecord]) -> Vec<&OperationalObservation> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!("statement counters") };
            row
        })
        .collect()
}

#[tokio::test]
async fn authenticated_statement_work_reuses_pure_explanations_and_keeps_native_rows_once_without_query_definitions() {
    for (source, rows) in statement_work_evidence_cases() {
        let native = source.observation.snapshot().unwrap();
        let outcome = SourceOutcome {
            source: native.sources[0].source.clone(),
            state: native.sources[0].state,
            reason: None,
            reason_message: None,
        };
        let expected = culprit_checks::statement_work::explain(&observations(&rows), Some(&outcome)).unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::statement_work(&capture).unwrap();
        assert_eq!(serde_json::to_value(&result.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        for row in rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row).unwrap();
            assert_eq!(result.context.records.iter().filter(|actual| actual.row == expected).count(), 1);
        }
        let json = serde_json::to_value(result).unwrap();
        assert!(json.get("snapshot").is_none() && json.get("workload").is_none() && json.get("findings").is_none());
    }
}

#[tokio::test]
async fn exact_native_counts_clocks_and_reset_qualification_survive_without_an_inferred_interval_or_delta() {
    let (source, rows) = statement_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::statement_work(&capture).unwrap();
    let first = result.report.entries.iter().find(|entry| entry.identity.query_id == "native-query-0").unwrap();
    assert_eq!(first.reset, ResetMarker::Unknown);
    assert_eq!(first.observed_at, "2020-01-01T12:00:00.100000000Z");
    let metrics = &first.work[0].metrics;
    assert!(metrics.iter().any(|metric| metric.text.as_deref() == Some("18446744073709551615")));
    assert!(metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    let zero = result.report.entries.iter().find(|entry| entry.identity.query_id == "native-query-1").unwrap();
    assert_eq!(zero.work[0].metrics.len(), 1);
    assert_eq!(zero.work[0].metrics[0].text.as_deref(), Some("0"));
    assert_eq!(zero.reset, ResetMarker::At("2020-01-01T10:00:00Z".into()));
}

#[tokio::test]
async fn archived_point_row_loss_remains_a_read_gap_without_upgrading_or_rewriting_native_coverage() {
    let (source, rows) = statement_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::statement_work(&capture).unwrap();
    assert_eq!(result.report.entries.len(), 1);
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::statement_work(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::NotPublished);
}

#[tokio::test]
async fn conflicting_subject_scope_clock_family_or_dataset_is_rejected_before_explanation() {
    for shape in ["shared-target", "scope", "clock", "dataset", "wrong-family"] {
        let (mut source, mut rows) = statement_work_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::StatementCounters(sample),
            ..
        }) = &mut rows[0]
        else {
            panic!()
        };
        match shape {
            "shared-target" => {
                source.target = CollectionTarget::Shared(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mysql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-namespace"),
                    native: NativeIdentity::new("native-instance"),
                })
            }
            "scope" => sample.scope = MeasurementScope::Server,
            "clock" => sample.observed_at = "2020-01-01T12:00:02Z".into(),
            "dataset" => {
                let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
                native.dataset = SnapshotDataset::Operational;
            }
            "wrong-family" => {}
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "wrong-family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::statement_work(&capture).unwrap_err();
        let problem = if matches!(shape, "scope" | "clock") {
            DeliveryProblem::Integrity
        } else {
            DeliveryProblem::UnsupportedSchema
        };
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(problem)), "{shape}");
    }
}
