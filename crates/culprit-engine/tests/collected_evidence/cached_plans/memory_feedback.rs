use super::*;
use culprit_collector::analytical::test_support::{MemoryFeedbackCase, memory_feedback_evidence};
#[cfg(feature = "mssql")]
use culprit_core::plan_facts::{
    PlanMemoryGrantFeedback, PlanMemoryGrantFeedbackIssue as Issue, PlanMemoryGrantFeedbackState as State,
};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn retained_feedback_preview_is_an_explicit_gap_until_exact_native_payload_is_requested() {
    let (source, records) = memory_feedback_evidence(MemoryFeedbackCase::DisabledUnstable);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(fixture.recovery_object_reads(), opens);
    let unavailable = collected_resources::cached_plans(&preview).unwrap();
    assert!(unavailable.report.entries.is_empty());
    assert_eq!(unavailable.report.sources[0].state, SourceState::Partial);
    assert_eq!(unavailable.context.source, source);

    let bytes = fixture.range.manifest().batches[0].bytes;
    let capture =
        fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes }).await;
    let result = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
    assert_eq!(capture.metrics().recovery_bytes, bytes);
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(result.context.source, source);
    assert_eq!(result.context.records.len(), 1);
    assert_eq!(
        result.context.records[0].row,
        ExportedRecord::new(&source, capture.capture(), records[0].clone()).unwrap()
    );
    let [entry] = result.report.entries.as_slice() else { panic!("one original retained plan") };
    assert_eq!(entry.definition_version, definition(&records[0]).version);
    #[cfg(not(feature = "mssql"))]
    {
        assert!(entry.plan.is_none());
        assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
    }
    #[cfg(feature = "mssql")]
    {
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(plan.kind, PlanKind::Actual);
        assert_eq!(plan.facts().memory_grant_feedback.as_ref().unwrap().state, State::DisabledUnstable);
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn native_feedback_states_and_exact_prior_grant_replay_through_the_same_selected_plan_parser_and_insights() {
    for case in MemoryFeedbackCase::ALL {
        if matches!(case, MemoryFeedbackCase::SourceUnavailable | MemoryFeedbackCase::PayloadUnavailable) {
            continue;
        }
        let (source, records) = memory_feedback_evidence(case);
        let native_record = definition(&records[0]);
        let ObjectDefinitionFact::MssqlRetainedPlan(native) = &native_record.fact else { panic!() };
        assert!(native.module_parameters.is_none());
        let direct = culprit_mssql::plan_xml::parse_retained_definition(native, &source).unwrap();
        let direct_insights = culprit_core::insights::insights(&direct);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        let result = collected_resources::cached_plans(&capture).unwrap();
        let [entry] = result.report.entries.as_slice() else { panic!("one selected statement") };
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(native.context.identity.clone())), "{case:?}");
        assert_eq!(plan.facts().memory_grant_feedback, direct.facts().memory_grant_feedback, "{case:?}");
        assert_eq!(plan.facts().memory_grant_feedback, expected_feedback(case), "{case:?}");
        assert_eq!(
            plan.kind,
            if case == MemoryFeedbackCase::Estimated { PlanKind::Estimated } else { PlanKind::Actual }
        );
        assert_eq!(
            result.report.sources[0].state,
            if case == MemoryFeedbackCase::PartialSource { SourceState::Partial } else { SourceState::Collected }
        );
        assert!(
            result.context.read_gaps.is_empty(),
            "native partial visibility is independent from warehouse completeness"
        );
        let expected = direct_insights.iter().find(|insight| insight.rule == "memory-grant-feedback");
        let observed =
            plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "memory-grant-feedback");
        assert_eq!(
            observed.is_some(),
            matches!(case, MemoryFeedbackCase::DisabledUnstable | MemoryFeedbackCase::LargePreviousGrant),
            "{case:?}"
        );
        assert_eq!(
            observed.map(|item| (&item.nodes, item.severity, item.impact)),
            expected.map(|item| (&item.nodes, item.severity, item.impact))
        );
        if let Some(insight) = observed {
            assert_eq!(insight.severity, culprit_core::Severity::Low);
            assert_eq!(insight.impact, 0.0);
            assert!(insight.fix_sql.is_none());
        }
        assert!(plan.raw.is_none() && plan.statement.is_none());
        assert!(
            plan.facts().retained.is_none(),
            "retained cache evidence supplies no historical execution-population authority"
        );
        assert!(plan.insights.as_ref().unwrap().items.iter().all(|insight| insight.fix_sql.is_none()));
        let report = serde_json::to_string(&result.report).unwrap();
        for private in [
            "<ShowPlanXML",
            "SELECT order_id FROM",
            "private-feedback-state-canary",
            "private-cache-machine-canary",
            "private-cache-instance-canary",
        ] {
            assert!(!report.contains(private), "{case:?}: {private}");
        }
        if case == MemoryFeedbackCase::LargePreviousGrant {
            assert!(report.contains("9007199254740993"), "exact native KB must survive binary64 boundaries");
            assert_eq!(
                plan.facts().memory_granted_kb,
                Some(65536.0),
                "previous execution grant never replaces current grant"
            );
        }
    }
}

#[tokio::test]
async fn missing_warehouse_feedback_and_native_unavailable_payloads_never_become_healthy_empty_plans() {
    for case in [MemoryFeedbackCase::SourceUnavailable, MemoryFeedbackCase::PayloadUnavailable] {
        let (source, records) = memory_feedback_evidence(case);
        let fixture = Fixture::new(source.clone(), records).await;
        let opens = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(fixture.recovery_object_reads(), opens);
        assert!(result.context.read_gaps.is_empty());
        assert_eq!(result.context.source, source);
        if case == MemoryFeedbackCase::SourceUnavailable {
            assert_eq!(result.report.sources[0].state, SourceState::Unavailable);
            assert!(result.report.entries.is_empty());
        } else {
            assert_eq!(result.report.sources[0].state, SourceState::Collected);
            let [entry] = result.report.entries.as_slice() else { panic!() };
            assert!(entry.plan.is_none());
            assert!(entry.limitations.iter().any(|limit| limit.reason
                == CachedPlanLimit::PayloadUnavailable {
                    payload: CachedPlanPayload::RetainedPlan,
                    reason: PayloadUnavailable::Restricted
                }));
        }
    }
    let (source, records) = memory_feedback_evidence(MemoryFeedbackCase::DisabledUnstable);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::ObjectDefinitions);
    let opens = fixture.recovery_object_reads();
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    let result = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(fixture.recovery_object_reads(), opens, "raw objects cannot replace missing warehouse membership");
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.sources[0].state, SourceState::Partial);
    assert_eq!(result.context.source, source);
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
}

#[cfg(feature = "mssql")]
fn expected_feedback(case: MemoryFeedbackCase) -> Option<PlanMemoryGrantFeedback> {
    use MemoryFeedbackCase::*;
    let (state, previous, issues) = match case {
        FirstExecution => (State::FirstExecution, Some("0"), vec![]),
        AccurateGrant => (State::AccurateGrant, Some("65536"), vec![]),
        DisabledUnstable | Estimated => (State::DisabledUnstable, Some("131072"), vec![]),
        Adjusting | PartialSource => (State::Adjusting, Some("131072"), vec![]),
        PercentileAdjusting => (State::PercentileAdjusting, Some("131072"), vec![]),
        Stable => (State::Stable, Some("65536"), vec![]),
        Unrecognized => (State::Unrecognized, Some("65536"), vec![]),
        InvalidState => (State::Unrecognized, Some("65536"), vec![Issue::InvalidState]),
        InvalidPreviousGrant => (State::Adjusting, None, vec![Issue::InvalidPreviousGrant]),
        LargePreviousGrant => (State::DisabledUnstable, Some("9007199254740993"), vec![]),
        Absent | SiblingOnly => return None,
        SourceUnavailable | PayloadUnavailable => unreachable!("no readable native plan"),
    };
    Some(PlanMemoryGrantFeedback {
        state,
        previous_grant_kb: previous.map(str::to_owned),
        runtime_observed: case != Estimated,
        issues,
    })
}
