use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{
        node_statement_work_evidence_cases, statement_outcome_evidence_cases, statement_work_evidence_cases,
    },
};
use culprit_core::{ErrorDetail, MeasurementScope, ResetMarker, SourceState, telemetry::*};
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
async fn exact_outcomes_reuse_the_same_pure_view_and_keep_same_digest_schemas_separate() {
    let mut keys = std::collections::BTreeMap::new();
    for (source, rows) in statement_outcome_evidence_cases() {
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = collected_resources::statement_work(&capture).unwrap();
        let expected = match &observations(&rows)[0].fact {
            OperationalFact::StatementCounters(sample) => sample.outcomes().unwrap(),
            _ => panic!(),
        };
        assert_eq!(analysis.report.entries[0].outcomes, expected);
        assert_eq!(
            analysis.report.entries[0].identity.qualifiers["schema"],
            source.target.database().unwrap().database
        );
        assert_eq!(analysis.report.entries[0].reset, ResetMarker::Unknown);
        assert_eq!(analysis.context.records.len(), 1);
        assert_eq!(fixture.recovery_object_reads(), opened);
        keys.insert(
            source.target.database().unwrap().database.clone(),
            analysis.report.entries[0].observation_key.clone(),
        );
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let partial = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let report = collected_resources::statement_work(&partial).unwrap().report;
        assert!(report.entries.is_empty());
        assert_eq!(report.source_state, Some(SourceState::Partial));
        assert_eq!(partial.source().observation.snapshot().unwrap().records, 1);
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
    assert_ne!(keys["app"], keys["other"]);
}

#[tokio::test]
async fn authenticated_statement_work_reuses_pure_explanations_and_keeps_native_rows_once_without_query_definitions() {
    for (source, rows) in statement_work_evidence_cases() {
        let snapshot =
            StatementCounterSnapshot::from_source(&source, observations(&rows).into_iter().cloned().collect()).unwrap();
        let expected = culprit_checks::statement_work::explain(&snapshot, true).unwrap();
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
        let receipt = CaptureIdentity { observation_id: "original-statement-counter-read".into(), revision: None };
        assert!(ExportedRecord::new(&source, &receipt, rows[0].clone()).is_ok());
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
        if matches!(shape, "shared-target" | "scope" | "clock" | "dataset") {
            // Invalid native facts never receive a receipt and cannot become an authenticated SDK fixture.
            let error = ExportedRecord::new(&source, &receipt, rows.remove(0)).unwrap_err();
            assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)), "{shape}");
            continue;
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "wrong-family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::statement_work(&capture).unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)), "{shape}");
    }
}

#[tokio::test]
async fn authenticated_node_points_keep_native_members_users_top_level_contexts_and_partial_visibility_separate() {
    for (source, rows) in node_statement_work_evidence_cases() {
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::statement_work(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert_eq!(result.report.entries.len(), rows.len());
        for row in observations(&rows) {
            let OperationalFact::StatementCounters(sample) = &row.fact else { panic!() };
            assert_eq!(sample.scope, MeasurementScope::Node);
            let entry = result.report.entries.iter().find(|entry| entry.observation_key == row.key).unwrap();
            assert_eq!(entry.identity, sample.identity);
            assert_eq!(entry.scope, sample.scope, "read ownership cannot replace the native counter population");
            assert_eq!(entry.reset, ResetMarker::Unknown);
            assert_eq!(entry.observed_at, sample.observed_at);
        }
        assert_eq!(result.report.source_state, Some(source.observation.snapshot().unwrap().sources[0].state));
        assert!(serde_json::to_value(result).unwrap().get("workload").is_none());
    }
}

#[test]
fn node_scope_and_native_source_identity_require_original_qualification_before_export() {
    for changed in
        ["target-only", "unknown-member", "unknown-epoch", "other-member", "user", "database", "top", "clock"]
    {
        let (mut source, mut rows) = node_statement_work_evidence_cases().remove(0);
        let receipt = CaptureIdentity { observation_id: "original-native-node-counter-read".into(), revision: None };
        assert!(ExportedRecord::new(&source, &receipt, rows[0].clone()).is_ok());
        match changed {
            "target-only" => source.partition = SourcePartition::Target,
            "unknown-member" => {
                source.partition =
                    SourcePartition::InstanceLocal { member: None, epoch: Some("native-process-a".into()) }
            }
            "unknown-epoch" => {
                source.partition = SourcePartition::InstanceLocal {
                    member: culprit_core::engines::ysql_node_member("0123456789abcdef0123456789abcdef"),
                    epoch: None,
                }
            }
            "other-member" => {
                source.partition = SourcePartition::InstanceLocal {
                    member: culprit_core::engines::ysql_node_member("11111111111111111111111111111111"),
                    epoch: Some("native-process-a".into()),
                }
            }
            _ => {
                let TelemetryRecord::Operational(observation) = &mut rows[0] else { panic!() };
                let OperationalFact::StatementCounters(sample) = &mut observation.fact else { panic!() };
                match changed {
                    "user" => {
                        sample.identity.qualifiers.insert("userid".into(), "01".into());
                    }
                    "database" => {
                        sample.identity.qualifiers.insert("database_oid".into(), "0".into());
                    }
                    "top" => {
                        sample.identity.qualifiers.insert("toplevel".into(), "unknown".into());
                    }
                    _ => sample.observed_at = "2020-01-01T12:00:02Z".into(),
                }
                observation.key = sample.key().unwrap();
                if matches!(changed, "user" | "database" | "top") {
                    // The native producer uses this source-owned helper before signing. Generic replay keeps
                    // opaque qualifiers; it does not reinterpret redacted identities as native OIDs/booleans.
                    assert!(
                        source.target.engine().spec().statement_counter_member(&sample.identity).is_none(),
                        "{changed}"
                    );
                    continue;
                }
            }
        }
        // Unknown source ownership can describe an incomplete source, but cannot qualify a known-node point.
        if matches!(changed, "unknown-member" | "unknown-epoch") {
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
            snapshot.sources[0] =
                StateSourceCoverage::observed(snapshot.sources[0].source.clone(), rows.len() as u32, false);
        }
        let error = ExportedRecord::new(&source, &receipt, rows.remove(0)).unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)), "{changed}");
    }
}

#[tokio::test]
async fn missing_archived_node_rows_preserve_original_counts_and_never_report_a_healthy_empty_population() {
    let (source, rows) = node_statement_work_evidence_cases().remove(0);
    let original = source.observation.snapshot().unwrap().sources[0].clone();
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::statement_work(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.report.entries.len(), original.records as usize - 1);
    assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0], original);
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::statement_work(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
}
