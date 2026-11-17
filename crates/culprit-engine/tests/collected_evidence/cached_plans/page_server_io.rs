use super::*;
use culprit_collector::analytical::test_support::{PageServerIoCase as Case, page_server_io_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn exact_page_server_corpus_keeps_original_definition_identity_scope_and_native_unknowns() {
    for case in Case::ALL {
        let (source, records) = page_server_io_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        culprit_collector::analytical::test_support::assert_object_definition_capture(&capture, &source, &records);
        let analysis = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(analysis.context.source, source, "{case:?}");
        assert_eq!(analysis.context.capture_manifest_sha256, capture.capture_manifest_sha256());
        assert_eq!(
            analysis.report.sources[0].state,
            match case {
                Case::PartialSource => SourceState::Partial,
                Case::SourceUnavailable => SourceState::Unavailable,
                _ => SourceState::Collected,
            },
            "{case:?}"
        );
        if case == Case::SourceUnavailable {
            assert!(analysis.report.entries.is_empty());
            continue;
        }
        let original = definition(&records[0]);
        let ObjectDefinitionFact::MssqlRetainedPlan(native) = &original.fact else { panic!() };
        let [entry] = analysis.report.entries.as_slice() else { panic!("one exact native definition: {case:?}") };
        assert_eq!(entry.definition_key, original.key);
        assert_eq!(entry.definition_version, original.version);
        assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(native.context.identity.clone())));
        #[cfg(feature = "mssql")]
        verify_plan(case, &source, native, entry);
        #[cfg(not(feature = "mssql"))]
        assert!(entry.plan.is_none());
        let encoded = serde_json::to_string(&analysis.report).unwrap();
        for private in
            ["private-page-sql-canary", "private-cache-machine-canary", "private-cache-instance-canary", "<ShowPlanXML"]
        {
            assert!(!encoded.contains(private), "{case:?}: {private}");
        }
    }
}

#[cfg(feature = "mssql")]
fn verify_plan(
    case: Case,
    source: &CollectionDescription,
    native: &MssqlRetainedPlanDefinition,
    entry: &culprit_core::CachedPlanEntry,
) {
    if matches!(case, Case::PayloadUnavailable | Case::MissingNativeStatement | Case::AmbiguousNativeStatement) {
        assert!(entry.plan.is_none(), "{case:?}");
        assert!(entry.limitations.iter().any(|limit| matches!(
            limit.reason,
            CachedPlanLimit::UnusablePlan | CachedPlanLimit::PayloadUnavailable { .. }
        )));
        return;
    }
    let direct = culprit_mssql::plan_xml::parse_retained_definition(native, source).unwrap();
    let plan = entry.plan.as_ref().unwrap();
    let io = |plan: &culprit_core::Plan| plan.root.runtime.as_ref().and_then(|runtime| runtime.page_server_io.clone());
    assert_eq!(io(plan), io(&direct), "same parser and exact original body: {case:?}");
    assert!(plan.raw.is_none() && plan.statement.is_none());
    assert!(plan.facts().retained.is_none() && plan.facts().last_run_at.is_none());
    assert!(!plan.props.contains_key("CachedExecutions"));
    assert_eq!(
        plan.kind,
        if matches!(case, Case::Estimated | Case::SiblingOnlyRuntime) { PlanKind::Estimated } else { PlanKind::Actual }
    );
    if matches!(case, Case::Estimated | Case::SiblingOnlyRuntime | Case::Absent) {
        assert!(io(plan).is_none());
        return;
    }
    let io = io(plan).unwrap();
    assert!(io.is_valid());
    let operator = plan.view.as_ref().unwrap().nodes.iter().find(|node| node.id == 7).unwrap();
    let presented = operator.page_server_io.as_ref().unwrap();
    assert_eq!(presented.metrics.len(), 4);
    assert!(presented.metrics.iter().all(|metric| metric.unit == culprit_core::Unit::Text));
    assert_eq!(
        presented.metrics[0].text,
        Some(io.reads.clone().unwrap_or_else(|| culprit_core::t!("view.plan_page_server_io.unknown")))
    );
    match case {
        Case::Complete | Case::UnqualifiedOwner | Case::PartialSource => {
            assert_eq!(io.reads.as_deref(), Some("9007199254740995"));
            assert_eq!(io.read_aheads.as_deref(), Some("7"));
            assert_eq!(io.lob_reads.as_deref(), Some("10"));
            assert_eq!(io.lob_read_aheads.as_deref(), Some("0"));
        }
        Case::Zero => assert_eq!(io.reads.as_deref(), Some("0")),
        Case::DuplicateThread => assert!(io.reads.is_none() && io.read_aheads.is_none() && io.lob_reads.is_none()),
        Case::PartialThreads | Case::InvalidCounter | Case::Overflow => {
            assert!(io.reads.is_none());
            assert_eq!(io.read_aheads.as_deref(), Some("0"));
            assert!(io.lob_reads.is_some());
        }
        _ => unreachable!(),
    }
    if case == Case::UnqualifiedOwner {
        assert!(!NativeReadContext::from_snapshot(source).unwrap().has_qualified_local_owner());
    }
    assert!(plan.insights.as_ref().unwrap().items.iter().all(|insight| insight.fix_sql.is_none()));
}

#[tokio::test]
async fn remote_counts_wait_for_exact_native_hydration_and_never_repair_missing_warehouse_membership() {
    let (source, records) = page_server_io_evidence(Case::Complete);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(fixture.recovery_object_reads(), opens);
    let analysis = collected_resources::cached_plans(&preview).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    culprit_collector::analytical::test_support::assert_object_definition_capture(&capture, &source, &records);
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
    assert_eq!(capture.metrics().recovery_objects, 1);
    fixture.remove_first_row(TelemetryStream::ObjectDefinitions);
    let opens = fixture.recovery_object_reads();
    let missing = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    assert_eq!(fixture.recovery_object_reads(), opens);
    let analysis = collected_resources::cached_plans(&missing).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert_eq!(analysis.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
}
