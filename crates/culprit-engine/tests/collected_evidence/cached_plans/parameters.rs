use super::*;
use culprit_collector::analytical::test_support::{ParameterEvidenceCase as Case, parameter_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

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
            matches!(case, Case::AccessMismatch | Case::PartialSource | Case::RecordedStatisticsContext),
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
