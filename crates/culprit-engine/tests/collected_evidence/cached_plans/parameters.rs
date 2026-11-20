use super::*;
use culprit_collector::analytical::test_support::{ParameterEvidenceCase as Case, parameter_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn original_scalar_access_and_independent_native_blocking_survive_recovery_without_exclusive_elapsed_attribution()
{
    use culprit_collector::analytical::{
        evidence::EvidenceCapture,
        test_support::{assert_activity_capture, scalar_blocking_activity_evidence},
    };
    fn blocking(capture: &EvidenceCapture) -> Option<culprit_core::Blocking> {
        // EvidenceReader already authenticated these exact original rows and source before this pure owner.
        let rows = capture
            .records()
            .iter()
            .map(|record| {
                let TelemetryRecord::Activity(observation) = &record.row().record else { panic!("original Activity") };
                ActivityFrameRow { observation, reference: None }
            })
            .collect::<Vec<_>>();
        let frame = ActivityFrameEvidence::new(capture.source(), &rows, capture.is_read_complete()).unwrap();
        culprit_core::blocking::analyze_activity(&frame).unwrap()
    }
    let (source, records) = parameter_evidence(Case::AccessMismatch);
    let plans = Fixture::new(source.clone(), records.clone()).await;
    let retained = plans.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    culprit_collector::analytical::test_support::assert_object_definition_capture(&retained, &source, &records);
    let (activity_source, activity_rows) = scalar_blocking_activity_evidence();
    assert_eq!(activity_source.target, source.target);
    assert_eq!(activity_source.partition, source.partition);
    let SourceObservation::Snapshot(plan_window) = &source.observation else { unreachable!() };
    let native = activity_source.observation.activity().unwrap();
    assert!(ObservationBounds::new(&native.observed_until, &plan_window.observed_from).is_some());
    assert_ne!(native.at, plan_window.observed_from);
    let actors = Fixture::new(activity_source.clone(), activity_rows.clone()).await;
    let activity = actors.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert_activity_capture(&activity, &activity_source, &activity_rows);
    let graph = blocking(&activity).unwrap();
    assert_eq!(graph.chains.len(), 1);
    let head = &graph.chains[0];
    assert_eq!(head.head_kind, culprit_core::HeadKind::IdleInTransaction);
    assert_eq!(head.tree.session, "62");
    assert!(
        head.tree.context.as_ref().unwrap().sql.is_none(),
        "the last/current statement is not a lock-acquisition statement"
    );
    assert_eq!(head.tree.children.len(), 1);
    assert_eq!(head.tree.children[0].session, "61");
    let before = collected_resources::cached_plans(&retained).unwrap();
    let expected = serde_json::to_value(&before).unwrap();
    let rendered = serde_json::to_string(&before.report).unwrap() + &serde_json::to_string(&graph).unwrap();
    for private in [
        "private-original-compiled-canary",
        "private-original-runtime-canary",
        "private-statement-canary",
        "ShowPlanXML",
        "ParameterRuntimeValue",
    ] {
        assert!(!rendered.contains(private), "{private}");
    }
    #[cfg(feature = "mssql")]
    {
        let plan = before.report.entries[0].plan.as_ref().unwrap();
        let parameter =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "parameter-context").unwrap();
        assert_eq!(parameter.severity, culprit_core::Severity::Low);
        assert_eq!(parameter.impact, 0.0);
        assert!(parameter.fix_sql.is_none());
        assert_eq!(parameter.fix, culprit_core::t!("insight.parameter_context.fix"));
        assert!(plan.raw.is_none() && plan.statement.is_none());
    }
    actors.remove_first_row(TelemetryStream::ActivityObservations);
    let missing = actors.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(!missing.is_read_complete());
    assert!(missing.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert!(
        blocking(&missing).is_none(),
        "a missing original waiter cannot borrow a later/retained plan or become a healthy-zero verdict"
    );
    plans.unpublish();
    actors.unpublish();
    assert_eq!(serde_json::to_value(collected_resources::cached_plans(&retained).unwrap()).unwrap(), expected);
    assert_eq!(serde_json::to_value(blocking(&activity)).unwrap(), serde_json::to_value(Some(graph)).unwrap());
}

#[tokio::test]
async fn original_optimizer_statistics_metadata_survives_recovery_without_current_statistics_or_exclusive_reuse_blame()
{
    let (source, records) = parameter_evidence(Case::RecordedStatisticsContext);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    culprit_collector::analytical::test_support::assert_object_definition_capture(&capture, &source, &records);
    let original = collected_resources::cached_plans(&capture).unwrap();
    let expected = serde_json::to_value(&original).unwrap();
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition(&capture.records()[0].row().record).fact else {
        panic!("original retained plan")
    };
    assert!(definition(&capture.records()[0].row().record).fact.native_plan_proofs_match(&source));
    let DefinitionPayload::Available { text, .. } = &native.batch_plan.payload else {
        panic!("authenticated original body")
    };
    assert!(text.contains("OptimizerStatsUsage") && text.contains("2020-01-01T11:45:00.003"));
    fixture.unpublish();
    let replayed = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(
        serde_json::to_value(&replayed).unwrap(),
        expected,
        "the exact original capture remains the evidence after the current warehouse head disappears"
    );
    #[cfg(feature = "mssql")]
    {
        let plan = replayed.report.entries[0].plan.as_ref().unwrap();
        let parameter =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "parameter-context").unwrap();
        assert_eq!(parameter.severity, culprit_core::Severity::Low);
        assert_eq!(parameter.impact, 0.0);
        assert!(parameter.fix_sql.is_none());
        assert!(plan.raw.is_none() && plan.statement.is_none());
    }
}

#[tokio::test]
async fn scalar_facts_wait_for_the_exact_native_body_and_missing_warehouse_membership_cannot_be_repaired() {
    let (source, records) = parameter_evidence(Case::AccessMismatch);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(fixture.recovery_object_reads(), opens);
    let result = collected_resources::cached_plans(&preview).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.sources[0].state, SourceState::Partial);
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    culprit_collector::analytical::test_support::assert_object_definition_capture(&capture, &source, &records);
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
    assert_eq!(capture.metrics().recovery_objects, 1);
    fixture.remove_first_row(TelemetryStream::ObjectDefinitions);
    let opens = fixture.recovery_object_reads();
    let missing = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    assert_eq!(fixture.recovery_object_reads(), opens);
    let result = collected_resources::cached_plans(&missing).unwrap();
    assert_eq!(result.context.source, source);
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.sources[0].state, SourceState::Partial);
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
}

#[tokio::test]
async fn original_scalar_comparison_access_and_visibility_replay_without_runtime_population_or_harm_claims() {
    for case in Case::ALL {
        let (source, records) = parameter_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        culprit_collector::analytical::test_support::assert_object_definition_capture(&capture, &source, &records);
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.context.source, source, "{case:?}");
        assert_eq!(
            result.report.sources[0].state,
            match case {
                Case::SourceUnavailable => SourceState::Unavailable,
                Case::PartialSource => SourceState::Partial,
                _ => SourceState::Collected,
            }
        );
        if case == Case::SourceUnavailable {
            assert!(result.report.entries.is_empty());
            continue;
        }
        let [entry] = result.report.entries.as_slice() else { panic!("one exact native definition: {case:?}") };
        #[cfg(not(feature = "mssql"))]
        {
            if case != Case::PayloadUnavailable {
                assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
            }
            assert!(entry.plan.is_none());
        }
        #[cfg(feature = "mssql")]
        verify_plan(case, &source, &records, entry);
        let encoded = serde_json::to_string(&result.report).unwrap();
        for private in [
            "private-original-compiled-canary",
            "private-original-runtime-canary",
            "private-statement-canary",
            "private-cache-machine-canary",
            "private-cache-instance-canary",
            "<ShowPlanXML",
        ] {
            assert!(!encoded.contains(private), "{case:?}: {private}");
        }
    }
}

#[cfg(feature = "mssql")]
fn verify_plan(
    case: Case,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
    entry: &culprit_core::CachedPlanEntry,
) {
    if case == Case::PayloadUnavailable {
        assert!(entry.plan.is_none());
        assert!(entry.limitations.iter().any(|limit| limit.reason
            == CachedPlanLimit::PayloadUnavailable {
                payload: CachedPlanPayload::RetainedPlan,
                reason: PayloadUnavailable::Restricted
            }));
        return;
    }
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition(&records[0]).fact else { panic!() };
    let direct = culprit_mssql::plan_xml::parse_retained_definition(native, source).unwrap();
    let plan = entry.plan.as_ref().unwrap();
    assert_eq!(plan.facts().parameter_context, direct.facts().parameter_context, "{case:?}");
    if let Some(proof) = &native.parameter_context_proof {
        assert_eq!(plan.facts().parameter_context.as_ref(), Some(&proof.context));
    }
    assert_eq!(
        plan.kind,
        if matches!(case, Case::Estimated | Case::SiblingOnlyRuntime) { PlanKind::Estimated } else { PlanKind::Actual }
    );
    assert!(plan.raw.is_none() && plan.statement.is_none());
    assert!(plan.facts().last_run_at.is_none() && plan.facts().retained.is_none());
    assert!(!plan.props.contains_key("CachedExecutions"));
    let insight = plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "parameter-context");
    let generic = matches!(
        case,
        Case::AccessMismatch
            | Case::AccurateAccess
            | Case::HealthyNativeVariant
            | Case::UnusedDifferentParameter
            | Case::UnsupportedConversion
            | Case::UseLimit
            | Case::PartialSource
            | Case::RecordedStatisticsContext
            | Case::RecordedGrantPressure
    );
    assert_eq!(insight.is_some(), generic, "{case:?}");
    if let Some(insight) = insight {
        let access = culprit_core::t!(
            "insight.parameter_context.access",
            node = "7",
            parameter = culprit_core::t!(
                "insight.parameter_context.named_type",
                parameter = "@customer",
                data_type = if case == Case::HealthyNativeVariant { "int" } else { "nvarchar(4000)" }
            )
        );
        assert_eq!(
            insight.detail.contains(&access),
            matches!(
                case,
                Case::AccessMismatch
                    | Case::PartialSource
                    | Case::RecordedStatisticsContext
                    | Case::RecordedGrantPressure
            ),
            "{case:?}"
        );
        assert_eq!(insight.impact, 0.0);
        assert!(insight.fix_sql.is_none());
    }
    assert!(plan.insights.as_ref().unwrap().items.iter().all(|insight| insight.fix_sql.is_none()));
}

#[cfg(feature = "mssql")]
#[test]
fn scalar_proof_cannot_move_to_another_scope_statement_node_or_native_body() {
    let (source, records) = parameter_evidence(Case::AccessMismatch);
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition(&records[0]).fact else { panic!() };
    for changed in ["scope", "member", "epoch", "generation", "sql-handle", "node", "comparison", "native-body"] {
        let mut source = source.clone();
        let mut native = native.clone();
        match changed {
            "scope" => {
                let CollectionTarget::Database(database) = &mut source.target else { panic!() };
                database.database = "another-database".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut source.partition else { panic!() };
                member.as_mut().unwrap().native = NativeIdentity::new("another-member");
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = Some("another-native-generation".into());
            }
            "generation" => native.context.identity.plan_generation = "99".into(),
            "sql-handle" => native.context.identity.sql_handle = "0xDEADBEEF".into(),
            "node" => native.parameter_context_proof.as_mut().unwrap().context.uses[0].node_id += 1,
            "comparison" => {
                native.parameter_context_proof.as_mut().unwrap().context.parameters[0].comparison =
                    culprit_core::plan_facts::PlanParameterComparison::SameRepresentation
            }
            "native-body" => native.batch_plan.native_sha256 = Some(digest(b"another-native-parameter-body")),
            _ => unreachable!(),
        }
        let error = culprit_mssql::plan_xml::parse_retained_definition(&native, &source).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{changed}");
    }
}

#[tokio::test]
async fn original_grant_queue_and_completed_scalar_work_recover_independently_without_exclusive_parameter_blame() {
    use culprit_collector::analytical::test_support::{
        assert_memory_participants_capture, scalar_grant_pressure_evidence,
    };
    let (plan_source, plans) = parameter_evidence(Case::RecordedGrantPressure);
    let plan_owner = Fixture::new(plan_source.clone(), plans.clone()).await;
    let retained = plan_owner.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    let (memory_source, grants) = scalar_grant_pressure_evidence();
    assert_eq!(memory_source.target, plan_source.target);
    assert_eq!(memory_source.partition, plan_source.partition);
    let earlier = memory_source.observation.snapshot().unwrap();
    let later = plan_source.observation.snapshot().unwrap();
    assert!(ObservationBounds::new(&earlier.observed_until, &later.observed_from).is_some());
    let memory_owner = Fixture::new(memory_source.clone(), grants.clone()).await;
    let captured = memory_owner.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert_memory_participants_capture(&captured, &memory_source, &grants);
    let memory = collected_resources::request_memory(&captured).unwrap();
    assert_eq!(memory.report.admission_resources.len(), 1);
    assert_eq!(memory.report.admission_resources[0].participants.len(), 2);
    assert!(
        memory.report.entries.iter().any(|entry| entry.mechanism
            == Some(culprit_core::MemoryParticipantMechanism::ReservationDuringClientOutputWait))
    );
    for message in [
        culprit_core::t!("view.request_memory.limit.ownership"),
        culprit_core::t!("view.request_memory.limit.clock"),
        culprit_core::t!("view.request_memory.limit.population"),
    ] {
        assert!(memory.report.limitations.contains(&message));
    }
    let completed = collected_resources::cached_plans(&retained).unwrap();
    #[cfg(feature = "mssql")]
    {
        let plan = completed.report.entries[0].plan.as_ref().unwrap();
        assert!(plan.facts().memory_grant_wait_ms.is_some_and(|wait| wait > 0.0));
        let insight =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "parameter-context").unwrap();
        assert_eq!(insight.severity, culprit_core::Severity::Low);
        assert_eq!(insight.impact, 0.0);
        assert!(insight.fix_sql.is_none());
        assert!(plan.raw.is_none() && plan.statement.is_none());
    }
    let expected_memory = serde_json::to_value(&memory).unwrap();
    let expected_plan = serde_json::to_value(&completed).unwrap();
    memory_owner.unpublish();
    plan_owner.unpublish();
    assert_eq!(serde_json::to_value(collected_resources::request_memory(&captured).unwrap()).unwrap(), expected_memory);
    assert_eq!(serde_json::to_value(collected_resources::cached_plans(&retained).unwrap()).unwrap(), expected_plan);
    let rendered = serde_json::to_string(&completed.report).unwrap() + &serde_json::to_string(&memory.report).unwrap();
    for private in [
        "private-original-compiled-canary",
        "private-original-runtime-canary",
        "private-statement-canary",
        "ShowPlanXML",
        "ParameterRuntimeValue",
    ] {
        assert!(!rendered.contains(private), "{private}");
    }
}

#[tokio::test]
async fn a_missing_original_grant_member_is_partial_history_and_cannot_be_repaired_by_the_completed_plan() {
    use culprit_collector::analytical::test_support::scalar_grant_pressure_evidence;
    let (source, rows) = scalar_grant_pressure_evidence();
    let owner = Fixture::new(source.clone(), rows).await;
    owner.remove_first_row(TelemetryStream::OperationalSamples);
    let original = owner.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!original.is_read_complete());
    let report = collected_resources::request_memory(&original).unwrap().report;
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.entries.len(), 1);
    assert!(report.entries.iter().all(|entry| entry.mechanism.is_none()));
    assert!(report.admission_resources.iter().all(|resource| resource.participants.len() == 1));
    let (plan_source, plans) = parameter_evidence(Case::RecordedGrantPressure);
    let plan_owner = Fixture::new(plan_source, plans).await;
    let retained = plan_owner.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    assert!(!collected_resources::cached_plans(&retained).unwrap().report.entries.is_empty());
    assert_eq!(
        collected_resources::request_memory(&original).unwrap().report,
        report,
        "a later completed plan cannot fill an original participant population"
    );
}

#[tokio::test]
async fn original_grant_unknown_owner_and_unavailable_statement_do_not_borrow_later_plan_authority() {
    use culprit_collector::analytical::test_support::scalar_grant_pressure_evidence;
    for unknown_owner in [false, true] {
        let (mut source, mut rows) = scalar_grant_pressure_evidence();
        if unknown_owner {
            source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            for coverage in &mut snapshot.sources {
                *coverage = StateSourceCoverage::observed(coverage.source.clone(), coverage.records, false);
            }
        }
        for row in &mut rows {
            let TelemetryRecord::Operational(observed) = row else { unreachable!() };
            let OperationalFact::ExecutionGrant(grant) = &mut observed.fact else { unreachable!() };
            if grant.state == ExecutionGrantState::Waiting {
                grant.request.as_mut().unwrap().statement =
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                grant.statement_relationship = GrantStatementRelationship::Unresolved;
                observed.key = grant.key().unwrap();
            }
        }
        let owner = Fixture::new(source, rows).await;
        let original = owner.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(original.is_read_complete());
        let report = collected_resources::request_memory(&original).unwrap().report;
        if unknown_owner {
            assert_eq!(report.source_state, Some(SourceState::Partial));
            assert!(report.entries.iter().all(|entry| entry.qualified_request.is_none() && entry.mechanism.is_none()));
            assert!(report.admission_resources.is_empty());
            assert!(report.limitations.contains(&culprit_core::t!("view.request_memory.limit.owner")));
        }
        let waiter = original
            .records()
            .iter()
            .find_map(|row| match &row.row().record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) if grant.state == ExecutionGrantState::Waiting => Some(grant.as_ref()),
                _ => None,
            })
            .unwrap();
        assert_eq!(waiter.statement_relationship, GrantStatementRelationship::Unresolved);
        assert!(matches!(
            waiter.request.as_ref().unwrap().statement,
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
        ));
        assert!(!serde_json::to_string(&report).unwrap().contains("SELECT order_id"));
    }
}
