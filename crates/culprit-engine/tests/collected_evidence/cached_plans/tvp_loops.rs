use super::*;
use culprit_collector::analytical::test_support::{TvpLoopCase, tvp_loop_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

fn retained(records: &[TelemetryRecord]) -> &MssqlRetainedPlanDefinition {
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition(&records[0]).fact else {
        panic!("one controlled native retained definition")
    };
    native
}

#[tokio::test]
async fn retained_tvp_loop_preview_stays_unavailable_until_the_exact_bounded_native_batch_is_hydrated() {
    let (source, records) = tvp_loop_evidence(TvpLoopCase::RepeatedAccess);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(preview.gaps().len(), 1);
    assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(fixture.recovery_object_reads(), opens);
    let report = collected_resources::cached_plans(&preview).unwrap();
    assert!(report.report.entries.is_empty());
    assert_eq!(report.report.sources[0].state, SourceState::Partial);
    assert_eq!(report.context.source, source);

    let bytes = fixture.range.manifest().batches[0].bytes;
    let capture =
        fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes }).await;
    assert!(capture.is_read_complete());
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(capture.metrics().recovery_bytes, bytes);
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
    let analysis = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), 1);
    assert_eq!(
        analysis.context.records[0].row,
        ExportedRecord::new(&source, capture.capture(), records[0].clone()).unwrap()
    );
    let [entry] = analysis.report.entries.as_slice() else { panic!("one native definition") };
    assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(retained(&records).context.identity.clone())));
    assert_eq!(entry.definition_version, definition(&records[0]).version);
    #[cfg(feature = "mssql")]
    assert!(
        entry.plan.as_ref().unwrap().insights.as_ref().unwrap().items.iter().any(|item| item.rule == "nested-loops")
    );
    #[cfg(not(feature = "mssql"))]
    {
        assert!(entry.plan.is_none());
        assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn archived_tvp_loop_cases_share_the_selected_native_parser_and_never_promote_lifetime_counts_or_partial_reads() {
    for case in TvpLoopCase::ALL {
        let (source, records) = tvp_loop_evidence(case);
        let native = retained(&records);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let bytes = fixture.range.manifest().batches[0].bytes;
        let capture = fixture
            .read_families_with_recovery(FAMILY, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes })
            .await;
        assert!(capture.is_read_complete(), "{case:?}");
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(native.statement_executions.as_deref(), Some("9007199254740993"));
        if case == TvpLoopCase::WrongBinding {
            let error = collected_resources::cached_plans(&capture).unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
            assert!(culprit_mssql::plan_xml::parse_retained_definition(native, &source).is_err());
            continue;
        }
        let direct = culprit_mssql::plan_xml::parse_retained_definition(native, &source)
            .unwrap_or_else(|error| panic!("direct retained parser failed for {case:?}: {error:?}"));
        let expected = culprit_core::insights::insights(&direct);
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert!(result.context.read_gaps.is_empty());
        assert_eq!(result.context.records.len(), 1);
        assert_eq!(
            result.report.sources[0].state,
            if case == TvpLoopCase::PartialSource { SourceState::Partial } else { SourceState::Collected }
        );
        let [entry] = result.report.entries.as_slice() else { panic!("one selected native statement") };
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(native.context.identity.clone())));
        assert_eq!(entry.definition_version, definition(&records[0]).version);
        assert_eq!(plan.kind, if case == TvpLoopCase::Estimated { PlanKind::Estimated } else { PlanKind::Actual });
        let expected = expected.iter().find(|insight| insight.rule == "nested-loops");
        let actual = plan.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "nested-loops");
        assert_eq!(
            actual.is_some(),
            matches!(
                case,
                TvpLoopCase::RepeatedAccess | TvpLoopCase::FilteredAndAggregated | TvpLoopCase::PartialSource
            ),
            "{case:?}"
        );
        assert_eq!(
            actual.map(|insight| (&insight.nodes, &insight.title, &insight.detail, &insight.fix, &insight.verify)),
            expected.map(|insight| (&insight.nodes, &insight.title, &insight.detail, &insight.fix, &insight.verify)),
            "{case:?}"
        );
        let input = plan.nodes().into_iter().find(|node| node.id == 1).unwrap();
        assert_eq!(
            input.facts().relational_input.unwrap().qualified_kind(),
            if matches!(case, TvpLoopCase::MissingBinding | TvpLoopCase::UnqualifiedFunctionAggregate) {
                RelationalInputKind::Unknown
            } else {
                RelationalInputKind::TableValuedParameter
            }
        );
        assert_eq!(
            entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput),
            matches!(case, TvpLoopCase::MissingBinding | TvpLoopCase::UnqualifiedFunctionAggregate)
        );
        if case == TvpLoopCase::UnqualifiedFunctionAggregate {
            assert!(native.relational_input_proof.is_none(), "unsupported COUNT_BIG(*) has no invented node proof");
            assert!(native.module_parameters.is_some(), "module metadata alone does not close the relation grammar");
            assert_eq!(plan.kind, PlanKind::Actual);
            assert_eq!(plan.root.act_rows, Some(1.0));
            assert_eq!(input.act_rows, Some(10_000.0));
            assert!(actual.is_none(), "actual counters cannot promote an unresolved input kind to a TVP");
        }
        if case == TvpLoopCase::FilteredAndAggregated {
            assert_eq!(plan.root.act_rows, Some(1.0));
            assert_eq!(input.act_rows, Some(10_000.0));
            assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20_000.0));
            let inner = plan.nodes().into_iter().find(|node| node.id == 2).unwrap();
            assert_eq!(inner.act_executions, Some(10_000.0));
            assert_eq!(inner.facts().serial_execution_count.as_deref(), Some("10000"));
            assert_eq!(inner.facts().access_rows.unwrap().actual_total, Some(2_000_000.0));
        }
        if case == TvpLoopCase::ParallelInner {
            let inner = plan.nodes().into_iter().find(|node| node.id == 2).unwrap();
            assert_eq!(inner.facts().parallel_threads, Some(1), "preserve the one recorded active worker");
            assert!(
                inner.facts().serial_execution_count.is_none(),
                "one active worker is not proof of serial execution"
            );
        }
        assert!(plan.raw.is_none() && plan.statement.is_none());
        assert!(
            plan.facts().retained.is_none(),
            "reader scope cannot establish the retained run's execution population"
        );
        assert!(!plan.props.contains_key("CachedExecutions"));
        assert!(plan.insights.as_ref().unwrap().items.iter().all(|insight| insight.fix_sql.is_none()));
        let serialized = serde_json::to_string(&result.report).unwrap();
        for private in ["<ShowPlanXML", "9264", "private-cache-machine-canary", "private-cache-instance-canary"] {
            assert!(!serialized.contains(private), "{case:?}: {private}");
        }
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn original_tvp_scope_and_module_proof_cannot_move_to_a_different_namespace_epoch_or_module() {
    let (source, records) = tvp_loop_evidence(TvpLoopCase::RepeatedAccess);
    let native = retained(&records);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    assert!(collected_resources::cached_plans(&capture).is_ok());
    let opens = fixture.recovery_object_reads();
    let mut other_database = source.clone();
    let CollectionTarget::Database(database) = &mut other_database.target else { panic!() };
    database.database = "another-database".into();
    let mut other_epoch = source.clone();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut other_epoch.partition else { panic!() };
    *epoch = Some("another-native-epoch".into());
    for shifted in [other_database, other_epoch] {
        let error = culprit_mssql::plan_xml::parse_retained_definition(native, &shifted).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
    let mut other_module = native.clone();
    other_module.module_parameters.as_mut().unwrap().object_id += 1;
    let error = culprit_mssql::plan_xml::parse_retained_definition(&other_module, &source).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery_object_reads(), opens, "scope validation performs no source I/O");
}
