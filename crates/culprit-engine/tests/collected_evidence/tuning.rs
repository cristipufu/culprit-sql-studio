use super::*;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::{self, TuningCase},
};
use culprit_core::SourceState;
use culprit_engine::collected_resources;

#[tokio::test]
async fn authenticated_tuning_points_reuse_the_pure_view_without_inventing_query_or_management_authority() {
    for (_, source, records) in test_support::tuning_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
        let analysis = collected_resources::tuning(&capture).unwrap();
        assert_eq!(analysis.context.records.len(), records.len());
        let rows = capture
            .records()
            .iter()
            .map(|record| match &record.row().record {
                TelemetryRecord::Tuning(observation) => observation,
                _ => panic!("tuning record"),
            })
            .collect::<Vec<_>>();
        let pure = culprit_checks::tuning::explain(&rows, &analysis.report.sources).unwrap();
        assert_eq!(serde_json::to_value(&pure).unwrap(), serde_json::to_value(&analysis.report).unwrap());
        assert_eq!(analysis.report.entries.len(), records.len());
        assert!(analysis.context.read_gaps.is_empty());
        let derived = serde_json::to_string(&analysis.report).unwrap();
        assert!(!derived.contains("EXEC ") && !derived.contains("OPTION(MAXDOP"));
        for entry in &analysis.report.entries {
            assert!(rows.iter().any(|row| row.source == entry.source && row.fact.key() == entry.observation_key));
        }
    }
}

#[tokio::test]
async fn missing_warehouse_rows_do_not_erase_native_failure_or_scope_context() {
    let (source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::TuningObservations);
    let capture = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    let analysis = collected_resources::tuning(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 3);
    assert!(!analysis.context.read_gaps.is_empty());
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert!(
        analysis
            .report
            .entries
            .iter()
            .any(|entry| entry.scope == Some(culprit_core::QueryStoreScope::ReplicaGroup { id: "7".into() }))
    );
}

#[tokio::test]
async fn large_tuning_bodies_require_explicit_bounded_recovery_and_never_become_commands() {
    let (source, records) = test_support::tuning_evidence(TuningCase::LargeDefinition);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let preview = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert!(!preview.is_read_complete());
    assert_eq!(fixture.recovery_object_reads(), before);
    let partial = collected_resources::tuning(&preview).unwrap();
    assert_eq!(partial.report.entries.len(), records.len() - 1);
    let full = fixture
        .read_families_with_recovery(&[EvidenceFamily::TuningObservations], RecoveryPayloadBudget::default())
        .await;
    test_support::assert_tuning_capture(&full, &source, &records);
    let complete = collected_resources::tuning(&full).unwrap();
    assert_eq!(complete.report.entries.len(), records.len());
    assert_eq!(full.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), before + 1);
    assert!(!serde_json::to_string(&complete.report).unwrap().contains("column_399"));
}

#[tokio::test]
async fn wrong_dataset_family_or_shared_subject_cannot_be_reinterpreted_as_tuning() {
    let (source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
    let (source, records) = test_support::object_definition_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
    let (mut source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let (shared, _) = test_support::shared_resource(SharedResourceKind::Instance, "native-instance");
    source.target = shared.target;
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
}

#[tokio::test]
async fn original_local_forcing_and_scalar_work_replay_as_independent_context_after_recovery() {
    use culprit_collector::analytical::test_support::{ParameterEvidenceCase, parameter_evidence};
    let (source, records) = test_support::tuning_evidence(TuningCase::LocalForcedRecovery);
    assert_eq!(source.target.database().unwrap().incarnation, None);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture
        .read_families_with_recovery(&[EvidenceFamily::TuningObservations], RecoveryPayloadBudget::default())
        .await;
    test_support::assert_tuning_capture(&capture, &source, &records);
    let tuning = collected_resources::tuning(&capture).unwrap();
    assert!(tuning.report.limitations.contains(&culprit_core::message!("view.tuning.limit.execution")));
    assert!(tuning.report.limitations.contains(&culprit_core::message!("view.tuning.limit.management")));
    let original_forcing = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::ForcedPlan(value), .. }) => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(original_forcing.plan_scope, culprit_core::QueryStoreScope::Local);
    assert_eq!(original_forcing.forcing.as_deref(), Some("MANUAL"));
    assert_eq!(original_forcing.reset, culprit_core::ResetMarker::Unknown);
    let (plan_source, plan_records) = parameter_evidence(ParameterEvidenceCase::AccessMismatch);
    assert_eq!(plan_source.target, source.target);
    assert_eq!(plan_source.partition, source.partition);
    let prior = source.observation.snapshot().unwrap();
    let later = plan_source.observation.snapshot().unwrap();
    assert!(ObservationBounds::new(&prior.observed_until, &later.observed_from).is_some());
    assert_ne!(
        prior.observed_from, later.observed_from,
        "matching owner does not invent the same clock or application"
    );
    let plans = Fixture::new(plan_source, plan_records).await;
    let plan_capture =
        plans.read_families_with_recovery(&[EvidenceFamily::ObjectDefinitions], RecoveryPayloadBudget::default()).await;
    let plan = collected_resources::cached_plans(&plan_capture).unwrap();
    #[cfg(feature = "mssql")]
    {
        let plan = plan.report.entries[0].plan.as_ref().unwrap();
        let parameter =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "parameter-context").unwrap();
        assert_eq!(parameter.severity, culprit_core::Severity::Low);
        assert_eq!(parameter.impact, 0.0);
        assert!(parameter.fix_sql.is_none());
        assert_eq!(parameter.fix, culprit_core::t!("insight.parameter_context.fix"));
    }
    let expected_tuning = serde_json::to_value(&tuning).unwrap();
    let expected_plan = serde_json::to_value(&plan).unwrap();
    fixture.unpublish();
    plans.unpublish();
    assert_eq!(serde_json::to_value(collected_resources::tuning(&capture).unwrap()).unwrap(), expected_tuning);
    assert_eq!(serde_json::to_value(collected_resources::cached_plans(&plan_capture).unwrap()).unwrap(), expected_plan);
    let rendered = serde_json::to_string(&tuning.report).unwrap() + &serde_json::to_string(&plan.report).unwrap();
    for private in [
        "private-original-compiled-canary",
        "private-original-runtime-canary",
        "private-statement-canary",
        "<ShowPlanXML",
        "EXEC ",
        "column_399",
    ] {
        assert!(!rendered.contains(private), "{private}");
    }
}

#[tokio::test]
async fn local_tuning_requires_its_vendor_and_complete_original_instance_owner() {
    for wrong in ["foreign-engine", "node", "unknown-epoch"] {
        let (mut source, records) = test_support::tuning_evidence(TuningCase::LocalForcedPlan);
        if wrong == "foreign-engine" {
            let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
            database.engine = culprit_core::Engine::Postgres;
        }
        let SourcePartition::InstanceLocal { member, epoch } = &mut source.partition else { unreachable!() };
        match wrong {
            "foreign-engine" => member.as_mut().unwrap().engine = culprit_core::Engine::Postgres,
            "node" => member.as_mut().unwrap().kind = SharedResourceKind::Node,
            "unknown-epoch" => *epoch = None,
            _ => unreachable!(),
        }
        if wrong == "unknown-epoch" {
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            for coverage in &mut snapshot.sources {
                coverage.complete_scope = false;
                coverage.state = culprit_core::SourceState::Partial;
            }
        }
        assert!(source.partition_is_consistent(), "{wrong} must reach the SDK as a coherent original source");
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
        assert!(collected_resources::tuning(&capture).is_err(), "{wrong}");
    }
}
