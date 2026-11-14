use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::runtime_distribution_evidence_cases,
};
use culprit_core::{ErrorDetail, MeasurementScope, ResetMarker, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn authenticated_distributions_reuse_the_pure_view_with_exact_native_populations_retained_once() {
    for (source, rows) in runtime_distribution_evidence_cases() {
        let outcomes = source
            .observation
            .snapshot()
            .unwrap()
            .sources
            .iter()
            .map(|coverage| SourceOutcome {
                source: coverage.source.clone(),
                state: coverage.state,
                reason: None,
                reason_message: None,
            })
            .collect::<Vec<_>>();
        let observations = rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::runtime_distributions::explain(&observations, &outcomes).unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::runtime_distributions(&capture).unwrap();
        assert_eq!(serde_json::to_value(&result.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        for row in rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row).unwrap();
            assert_eq!(result.context.records.iter().filter(|record| record.row == expected).count(), 1);
        }
        let output = serde_json::to_value(result).unwrap();
        assert!(
            output.get("snapshot").is_none() && output.get("workload").is_none() && output.get("findings").is_none()
        );
        assert!(output["report"].get("histogram").is_none());
    }
}

#[tokio::test]
async fn native_summary_and_later_histogram_keep_different_counts_clocks_and_unknown_epochs() {
    let (source, rows) = runtime_distribution_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::runtime_distributions(&capture).unwrap();
    assert_eq!(result.report.entries.len(), 2);
    let mut clocks = std::collections::BTreeSet::new();
    for entry in &result.report.entries {
        assert_eq!(entry.reset, ResetMarker::Unknown);
        clocks.insert(entry.observed_at.clone());
    }
    assert_eq!(clocks.len(), 2);
    let native = result
        .context
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::RuntimeDistribution(point),
                ..
            }) = &record.row.record
            else {
                panic!()
            };
            point.as_ref()
        })
        .collect::<Vec<_>>();
    let summary = native.iter().find(|point| point.distribution.source == "native-summary").unwrap();
    let histogram = native.iter().find(|point| point.distribution.source == "native-histogram").unwrap();
    assert_eq!(summary.distribution.population.sample_count(), None);
    assert_eq!(histogram.distribution.population.sample_count(), Some(1000));
    assert_eq!(
        histogram.distribution.histogram[1].upper,
        culprit_core::RuntimeDuration::Picoseconds { value: u64::MAX.to_string() }
    );
    assert_eq!(
        summary.identity, histogram.identity,
        "a shared identity never combines independently observed populations"
    );
}

#[tokio::test]
async fn source_unavailability_and_exact_empty_capture_remain_distinct_without_healthy_absence() {
    for (source, rows) in runtime_distribution_evidence_cases().into_iter().skip(2) {
        let expected = source.observation.snapshot().unwrap().sources[0].state;
        let fixture = Fixture::new(source, rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::runtime_distributions(&capture).unwrap();
        assert!(result.report.entries.is_empty());
        assert_eq!(result.report.sources[0].state, expected);
        assert!(result.report.summary.contains("does not establish"));
        assert!(result.context.read_gaps.is_empty());
    }
}

#[tokio::test]
async fn missing_rows_and_missing_publication_stay_store_gaps_without_rewriting_native_source_coverage() {
    let (source, rows) = runtime_distribution_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::runtime_distributions(&capture).unwrap();
    assert_eq!(result.report.entries.len(), 1);
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert!(result.report.sources.iter().all(|source| source.state == SourceState::Partial));
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::runtime_distributions(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::NotPublished);
}

#[tokio::test]
async fn warehouse_gaps_preserve_each_native_sources_unavailability_or_inapplicability() {
    for expected in [SourceState::Unavailable, SourceState::NotApplicable] {
        let (mut source, rows) = runtime_distribution_evidence_cases().remove(1);
        let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
        native.sources[1] = match expected {
            SourceState::Unavailable => StateSourceCoverage::unavailable(
                "native-histogram".into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
            SourceState::NotApplicable => StateSourceCoverage::not_applicable("native-histogram".into()),
            _ => unreachable!(),
        };
        let fixture = Fixture::new(source.clone(), rows).await;
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        for unpublished in [false, true] {
            if unpublished {
                fixture.unpublish();
            }
            let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
            assert!(!capture.is_read_complete());
            let result = collected_resources::runtime_distributions(&capture).unwrap();
            assert!(result.report.entries.is_empty());
            assert_eq!(
                result.report.sources.iter().find(|source| source.source == "native-histogram").unwrap().state,
                expected
            );
            assert_eq!(
                result.report.sources.iter().find(|source| source.source == "retained-native-runs").unwrap().state,
                SourceState::Partial
            );
            let expected_gap =
                if unpublished { EvidenceGapReason::NotPublished } else { EvidenceGapReason::MissingRows };
            assert!(result.context.read_gaps.iter().any(|gap| gap.reason == expected_gap));
            assert_eq!(result.context.source, source, "native qualification remains independently authenticated");
        }
    }
}

#[tokio::test]
async fn wrong_dataset_family_subject_scope_or_capture_clock_is_rejected() {
    for shape in ["dataset", "family", "subject", "scope", "clock"] {
        let (mut source, mut rows) = runtime_distribution_evidence_cases().remove(0);
        match shape {
            "dataset" => {
                let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
                native.dataset = SnapshotDataset::Operational;
            }
            "subject" => {
                source.target = CollectionTarget::Shared(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mysql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("namespace"),
                    native: NativeIdentity::new("instance"),
                })
            }
            "scope" | "clock" => {
                let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
                let OperationalFact::RuntimeDistribution(point) = &mut row.fact else { panic!() };
                if shape == "scope" {
                    point.scope = MeasurementScope::Server;
                } else {
                    point.observed_at = "2020-01-01T12:00:02Z".into();
                }
                row.key = point.key().unwrap();
            }
            "family" => {}
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::runtime_distributions(&capture).unwrap_err();
        let expected = if matches!(shape, "scope" | "clock") {
            DeliveryProblem::Integrity
        } else {
            DeliveryProblem::UnsupportedSchema
        };
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(expected)), "{shape}");
    }
}
