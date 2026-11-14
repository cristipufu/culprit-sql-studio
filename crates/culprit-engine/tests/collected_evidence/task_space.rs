use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::task_space_evidence_cases,
};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn observations(rows: &[TelemetryRecord]) -> Vec<&TaskSpaceObservation> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::TaskSpace(row), .. }) =
                row
            else {
                panic!("native task fixture")
            };
            row.as_ref()
        })
        .collect()
}

#[tokio::test]
async fn authenticated_task_investigation_reuses_pure_explanations_and_keeps_original_rows_once() {
    for (source, rows) in task_space_evidence_cases() {
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let expected = culprit_checks::task_space::explain(
            &observations(&rows),
            Some(&SourceOutcome {
                source: coverage.source.clone(),
                state: coverage.state,
                reason: None,
                reason_message: None,
            }),
        )
        .unwrap();
        let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let database = source.target.database().unwrap();
        scan.server.database = database.database.clone();
        scan.identity = Some(database.clone());
        scan.sources.retain(|source| source.source != coverage.source);
        scan.sources.push(SourceOutcome {
            source: coverage.source.clone(),
            state: coverage.state,
            reason: None,
            reason_message: None,
        });
        let bounds = source.observation.snapshot().unwrap();
        scan.operations.task_space = Some(TaskSpaceSnapshot {
            source: coverage.source.clone(),
            observed_from: bounds.observed_from.clone(),
            observed_until: bounds.observed_until.clone(),
            observations: observations(&rows).into_iter().cloned().collect(),
        });
        assert_eq!(culprit_checks::analyze(scan).task_space.unwrap(), expected);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::task_space(&capture).unwrap();
        assert_eq!(result.report, expected);
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        for row in observations(&rows) {
            assert!(
                result
                    .report
                    .entries
                    .iter()
                    .any(|entry| entry.observation_key == row.key().unwrap() && entry.task == row.task)
            );
        }
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(encoded["report"].get("observations").is_none(), "native records live once in authenticated context");
        assert!(!serde_json::to_string(&result.report).unwrap().contains("SELECT note"));
    }
}

#[tokio::test]
async fn task_counter_exactness_and_unqualified_context_survive_readback_without_allocator_or_delta_inference() {
    let (source, rows) = task_space_evidence_cases().remove(1);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::task_space(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    let recorded = result
        .context
        .records
        .iter()
        .filter_map(|row| match &row.row.record {
            TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::TaskSpace(row), .. }) => {
                Some(row)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(recorded.iter().any(|row| row.user_allocated_pages.as_deref() == Some("9223372036854775807")));
    assert!(
        recorded
            .iter()
            .any(|row| row.user_deallocated_pages.as_deref() == Some("0") && row.internal_allocated_pages.is_none())
    );
    let changed = recorded.iter().find(|row| row.request_context() == TaskRequestContext::Changed).unwrap();
    assert!(!changed.request.as_ref().unwrap().statement.is_available());
    assert!(changed.statement_identity.is_none() && changed.plan_identity.is_none());
    assert!(recorded.iter().any(|row| row.request.is_none()));
}

#[tokio::test]
async fn task_read_gaps_preserve_native_coverage_and_never_produce_a_complete_empty_report() {
    let (source, rows) = task_space_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::task_space(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::task_space(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
}

#[tokio::test]
async fn retrieving_every_stored_task_row_cannot_upgrade_partial_native_scope_even_when_empty() {
    let cases = task_space_evidence_cases();
    for state in [SourceState::Partial, SourceState::Empty] {
        let (mut source, rows) = cases
            .iter()
            .find(|(source, _)| source.observation.snapshot().unwrap().sources[0].state == state)
            .cloned()
            .expect("positive partial and empty native captures");
        let SourceObservation::Snapshot(native) = &mut source.observation else { panic!("point capture") };
        native.sources[0] = StateSourceCoverage::observed(native.sources[0].source.clone(), native.records, false);
        assert!(native.is_valid());
        let fixture = Fixture::new(source, rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "all stored rows and their publication proof were retrieved");
        let result = collected_resources::task_space(&capture).unwrap();
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert_eq!(result.report.entries.len(), rows.len());
        assert_eq!(result.context.records.len(), rows.len());
        assert!(result.context.read_gaps.is_empty());
        assert!(!result.context.source.observation.snapshot().unwrap().sources[0].complete_scope);
    }
}

#[tokio::test]
async fn task_analysis_rejects_wrong_dataset_subject_scope_and_independent_check_clocks() {
    for shape in ["dataset", "shared-target", "family", "database", "counter-clock", "check-clock"] {
        let (mut source, mut rows) = task_space_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::TaskSpace(row), .. }) =
            &mut rows[0]
        else {
            panic!()
        };
        match shape {
            "dataset" => {
                let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
                native.dataset = SnapshotDataset::Operational;
            }
            "shared-target" => {
                source.target = CollectionTarget::Shared(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-host"),
                    native: NativeIdentity::new("native-instance"),
                })
            }
            "family" => {}
            "database" => row.request.as_mut().unwrap().database = Some("another-db".into()),
            "counter-clock" => row.observed_at = "2020-01-01T12:00:02Z".into(),
            "check-clock" => row.after_check.as_mut().unwrap().observed_at = "2020-01-01T12:00:02Z".into(),
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        assert!(collected_resources::task_space(&capture).is_err(), "{shape}");
    }
}
