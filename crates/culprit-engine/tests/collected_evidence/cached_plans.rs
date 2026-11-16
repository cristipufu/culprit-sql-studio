use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::{
        memory_failure_evidence, object_definition_bound_join_evidence, object_definition_evidence_cases,
        object_definition_hydration_evidence, object_definition_retained_input_evidence,
    },
};
#[cfg(feature = "mssql")]
use culprit_core::plan_facts::RelationalInputKind;
use culprit_core::{
    CachedPlanIdentity, CachedPlanLimit, CachedPlanPayload, Engine, MeasurementScope, PlanKind, SourceState,
    telemetry::*,
};
use culprit_engine::collected_resources;

fn case(index: usize) -> (CollectionDescription, Vec<TelemetryRecord>) {
    object_definition_evidence_cases().swap_remove(index)
}

fn definition(record: &TelemetryRecord) -> &ObjectDefinitionRecord {
    let TelemetryRecord::ObjectDefinition(definition) = record else { panic!("object definition") };
    definition
}

fn replace_fact(record: &mut TelemetryRecord, change: impl FnOnce(&mut ObjectDefinitionFact)) {
    let TelemetryRecord::ObjectDefinition(definition) = record else { panic!("object definition") };
    let mut fact = definition.fact.clone();
    change(&mut fact);
    *definition = ObjectDefinitionRecord::new(definition.source.clone(), fact).unwrap();
}

#[cfg(feature = "mssql")]
fn payload(raw: &str) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
        native_sha256: Some(digest(raw.as_bytes())),
        payload: DefinitionPayload::plan(raw, Engine::Mssql),
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn archived_cached_plan_uses_original_module_kind_without_manufacturing_runtime_evidence() {
    let (source, records) = case(0);
    let original = definition(&records[0]);
    let ObjectDefinitionFact::MssqlCachedDefinition(native) = &original.fact else { panic!() };
    let identity = native.context.identity.clone();
    let key = original.key.clone();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let result = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(result.context.source, source);
    assert_eq!(result.context.records.len(), 3);
    let entry = result.report.entries.iter().find(|entry| entry.definition_key == key).unwrap();
    assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(identity)));
    let plan = entry.plan.as_ref().unwrap();
    assert_eq!(plan.kind, PlanKind::Estimated);
    assert_eq!(plan.root.est_rows, 100.0);
    assert_eq!(plan.root.act_rows, None);
    assert_eq!(plan.root.facts().relational_input.unwrap().qualified_kind(), RelationalInputKind::TableValuedParameter);
    assert!(plan.raw.is_none() && plan.statement.is_none());
    assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingActualMeasurements));
    assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingCurrentPrerequisites));
    assert!(!plan.insights.as_ref().unwrap().items.iter().any(|insight| insight.rule == "misestimate"));
    let report = serde_json::to_string(&result.report).unwrap();
    assert!(!report.contains("<ShowPlanXML") && !report.contains("SELECT value FROM"));
    assert!(!report.contains("9264"));
}

#[tokio::test]
async fn explicit_hydration_restores_large_native_plans_and_original_tvp_context_without_borrowed_runtime() {
    let (source, records) = object_definition_hydration_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let reads_before = fixture.recovery_object_reads();
    let preview = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(fixture.recovery_object_reads(), reads_before);
    let preview_analysis = collected_resources::cached_plans(&preview).unwrap();
    assert!(preview_analysis.report.entries.is_empty());
    assert_eq!(preview_analysis.report.sources[0].state, SourceState::Partial);

    let batch = &fixture.range.manifest().batches[0];
    let capture = fixture
        .read_families_with_recovery(
            &[EvidenceFamily::ObjectDefinitions],
            RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes },
        )
        .await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(capture.metrics().recovery_bytes, batch.bytes);
    assert_eq!(fixture.recovery_object_reads() - reads_before, 1);
    let analysis = collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), records.len());
    assert_eq!(analysis.report.entries.len(), records.len());
    assert_eq!(analysis.report.sources[0].state, SourceState::Collected);
    assert_eq!(records.len(), 2);

    #[cfg(feature = "mssql")]
    let mut expected_cartesian = [false, true].into_iter();
    #[cfg(feature = "mssql")]
    let mut expected_inputs = [RelationalInputKind::TableValuedParameter, RelationalInputKind::Unknown].into_iter();
    for record in records {
        let native_definition = definition(&record);
        let expected = ExportedRecord::new(&source, capture.capture(), record.clone()).unwrap();
        assert_eq!(analysis.context.records.iter().filter(|row| row.row == expected).count(), 1);
        let entry = analysis.report.entries.iter().find(|entry| entry.definition_key == native_definition.key).unwrap();
        let ObjectDefinitionFact::MssqlCachedDefinition(native) = &native_definition.fact else { panic!() };
        assert_eq!(entry.definition_version, native_definition.version);
        assert_eq!(entry.identity, Some(CachedPlanIdentity::Mssql(native.context.identity.clone())));
        let DefinitionPayload::Available { text, .. } = &native.estimated_plan.payload else { panic!() };
        assert!(text.len() > 1024);
        assert!(native.module_parameters.is_some());

        #[cfg(feature = "mssql")]
        {
            let direct = culprit_mssql::plan_xml::parse_cached_definition(native, capture.source()).unwrap();
            let direct_insights = culprit_core::insights::insights(&direct);
            let plan = entry.plan.as_ref().unwrap();
            assert_eq!(plan.kind, PlanKind::Estimated);
            assert!(plan.nodes().iter().all(|node| node.act_rows.is_none() && node.act_executions.is_none()));
            let inputs = plan
                .nodes()
                .iter()
                .filter_map(|node| node.facts().relational_input.map(|input| input.qualified_kind()))
                .collect::<Vec<_>>();
            let expected_kind = expected_inputs.next().unwrap();
            assert_eq!(inputs, [expected_kind]);
            assert_eq!(plan.facts().elapsed_ms, None);
            assert_eq!(plan.facts().last_run_at, None);
            assert!(!plan.props.contains_key("CachedExecutions"));
            assert!(plan.raw.is_none() && plan.statement.is_none());
            let insights = &plan.insights.as_ref().unwrap().items;
            let cartesian = expected_cartesian.next().unwrap();
            assert_eq!(
                direct.nodes().iter().any(|node| node.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate)),
                cartesian
            );
            assert_eq!(
                plan.nodes().iter().any(|node| node.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate)),
                cartesian
            );
            if cartesian {
                let expected = direct_insights.iter().find(|insight| insight.rule == "no-join-predicate").unwrap();
                let observed = insights.iter().find(|insight| insight.rule == "no-join-predicate").unwrap();
                assert_eq!(observed.nodes, expected.nodes);
                assert_eq!(observed.detail, expected.detail);
            } else {
                assert!(!insights.iter().any(|insight| insight.rule == "no-join-predicate"));
            }
            assert_eq!(
                entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput),
                expected_kind == RelationalInputKind::Unknown
            );
            assert!(!insights.iter().any(|insight| insight.rule == "misestimate"));
            assert!(insights.iter().all(|insight| insight.fix_sql.is_none()));
            assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingActualMeasurements));
            assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingCurrentPrerequisites));
        }
        #[cfg(not(feature = "mssql"))]
        {
            assert!(entry.plan.is_none());
            assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
        }
    }
    let report = serde_json::to_string(&analysis.report).unwrap();
    assert!(
        !report.contains("<ShowPlanXML") && !report.contains("SELECT r.value") && !report.contains("SELECT value FROM")
    );
    assert!(!report.contains("9264"));
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn redacted_join_namespaces_cannot_be_reconstructed_from_an_authenticated_capture_target() {
    let (original_source, original_records) = object_definition_hydration_evidence();
    let original_key = definition(&original_records[1]).key.clone();
    for evidence in [
        "original",
        "other-capture-database",
        "capture-database-named-database",
        "missing-native-database",
        "missing-module",
    ] {
        let mut source = original_source.clone();
        let mut records = original_records.clone();
        if matches!(evidence, "other-capture-database" | "capture-database-named-database") {
            let CollectionTarget::Database(database) = &mut source.target else { panic!() };
            database.database =
                if evidence == "capture-database-named-database" { "database" } else { "other-captured-database" }
                    .into();
        }
        replace_fact(&mut records[1], |fact| {
            let ObjectDefinitionFact::MssqlCachedDefinition(native) = fact else { panic!() };
            if evidence == "missing-module" {
                native.module_parameters = None;
            } else if evidence == "missing-native-database" {
                let DefinitionPayload::Available { text, .. } = &native.estimated_plan.payload else { panic!() };
                let changed = text.replace("Database=\"[database]\"", "");
                assert_ne!(&changed, text, "the counterexample must change the original native namespace");
                native.estimated_plan = payload(&changed);
            }
        });
        assert_eq!(definition(&records[1]).key, original_key);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let reads_before = fixture.recovery_object_reads();
        let batch = &fixture.range.manifest().batches[0];
        let capture = fixture
            .read_families_with_recovery(
                &[EvidenceFamily::ObjectDefinitions],
                RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes },
            )
            .await;
        assert!(capture.is_read_complete(), "{evidence}: {:?}", capture.gaps());
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(fixture.recovery_object_reads() - reads_before, 1);
        let analysis = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.sources[0].state, SourceState::Collected);
        let entry = analysis.report.entries.iter().find(|entry| entry.definition_key == original_key).unwrap();
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(
            plan.root.children[0].facts().relational_input.unwrap().qualified_kind(),
            RelationalInputKind::Unknown,
            "a redacted Object.Database is not the original namespace: {evidence}"
        );
        assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput));
        assert!(plan.root.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate));
        let insights = &plan.insights.as_ref().unwrap().items;
        assert!(insights.iter().any(|insight| insight.rule == "no-join-predicate"));
        assert!(!insights.iter().any(|insight| insight.rule == "misestimate"));
        assert!(plan.nodes().iter().all(|node| node.act_rows.is_none() && node.act_executions.is_none()));
        assert!(plan.raw.is_none() && plan.statement.is_none());
        let original = ExportedRecord::new(&source, capture.capture(), records[1].clone()).unwrap();
        assert!(analysis.context.records.iter().any(|row| row.row == original));
        let direct_key = &definition(&records[0]).key;
        let direct = analysis.report.entries.iter().find(|entry| &entry.definition_key == direct_key).unwrap();
        assert_eq!(
            direct.plan.as_ref().unwrap().root.facts().relational_input.unwrap().qualified_kind(),
            RelationalInputKind::TableValuedParameter,
            "independent direct-module proof remains valid: {evidence}"
        );
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn retained_native_input_proofs_restore_exact_archived_join_kinds_and_independent_warnings() {
    for (source, records) in object_definition_bound_join_evidence() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let reads_before = fixture.recovery_object_reads();
        let preview = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        assert!(preview.records().is_empty() && !preview.is_read_complete());
        let batch = &fixture.range.manifest().batches[0];
        let capture = fixture
            .read_families_with_recovery(
                &[EvidenceFamily::ObjectDefinitions],
                RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes },
            )
            .await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(fixture.recovery_object_reads() - reads_before, 1);
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records[0].row.record, records[0]);
        let [entry] = result.report.entries.as_slice() else { panic!() };
        let plan = entry.plan.as_ref().unwrap();
        match &definition(&records[0]).fact {
            ObjectDefinitionFact::MssqlRetainedPlan(native) => {
                assert_eq!(native.statement_executions.as_deref(), Some("9007199254740993"));
                assert_eq!(plan.kind, PlanKind::Actual);
                assert_eq!(plan.root.children[0].facts().access_rows.unwrap().actual_total, Some(20000.0));
                assert_eq!(plan.root.children[0].act_rows, Some(10000.0));
                assert!(
                    plan.root.children[0].facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan()
                );
                assert_eq!(plan.root.act_rows, None);
                assert_eq!(plan.root.children[1].act_rows, None);
                assert_eq!(plan.facts().last_run_at, None);
                assert_eq!(plan.facts().elapsed_ms, None);
            }
            ObjectDefinitionFact::MssqlCachedDefinition(_) => {
                assert_eq!(plan.kind, PlanKind::Estimated);
                assert!(plan.nodes().iter().all(|node| node.act_rows.is_none()));
            }
            _ => panic!(),
        }
        assert_join_proof_explanation(plan, entry);
    }
}

#[cfg(feature = "mssql")]
fn assert_join_proof_explanation(plan: &culprit_core::Plan, entry: &culprit_core::CachedPlanEntry) {
    assert_eq!(
        plan.root.children[0].facts().relational_input.unwrap().qualified_kind(),
        RelationalInputKind::TableValuedParameter
    );
    assert!(!entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput));
    assert!(plan.root.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate));
    let insights = &plan.insights.as_ref().unwrap().items;
    assert!(insights.iter().any(|insight| insight.rule == "no-join-predicate"));
    let completion =
        culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K");
    assert!(insights.iter().all(|insight| !insight.detail.contains(&completion) && insight.fix_sql.is_none()));
    assert!(plan.raw.is_none() && plan.statement.is_none());
    assert!(!plan.props.contains_key("CachedExecutions"));
    let report = serde_json::to_string(entry).unwrap();
    assert!(!report.contains("private-cache-machine-canary") && !report.contains("private-cache-instance-canary"));
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn omitted_proof_stays_unknown_while_a_false_self_consistent_node_proof_is_an_integrity_error() {
    for (source, original) in object_definition_bound_join_evidence() {
        for wrong_node in [false, true] {
            let mut records = original.clone();
            replace_fact(&mut records[0], |fact| match fact {
                ObjectDefinitionFact::MssqlCachedDefinition(native) => {
                    let mut bindings = native.relational_input_proof.take().unwrap().bindings;
                    if wrong_node {
                        bindings[0].node_id = 999;
                        native.relational_input_proof =
                            Some(MssqlRelationalInputProof::for_cached(&source, native, bindings).unwrap());
                    }
                }
                ObjectDefinitionFact::MssqlRetainedPlan(native) => {
                    let mut bindings = native.relational_input_proof.take().unwrap().bindings;
                    if wrong_node {
                        bindings[0].node_id = 999;
                        native.relational_input_proof =
                            Some(MssqlRelationalInputProof::for_retained(&source, native, bindings).unwrap());
                    }
                }
                _ => panic!(),
            });
            let fixture = Fixture::new(source.clone(), records).await;
            let batch = &fixture.range.manifest().batches[0];
            let capture = fixture
                .read_families_with_recovery(
                    &[EvidenceFamily::ObjectDefinitions],
                    RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes },
                )
                .await;
            if wrong_node {
                let error = collected_resources::cached_plans(&capture).unwrap_err();
                assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
            } else {
                let analysis = collected_resources::cached_plans(&capture).unwrap();
                let entry = &analysis.report.entries[0];
                let plan = entry.plan.as_ref().unwrap();
                assert_eq!(
                    plan.root.children[0].facts().relational_input.unwrap().qualified_kind(),
                    RelationalInputKind::Unknown
                );
                assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput));
                assert!(
                    plan.insights.as_ref().unwrap().items.iter().any(|insight| insight.rule == "no-join-predicate")
                );
            }
        }
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn retained_actual_rows_never_borrow_the_cached_modules_kind_or_execution_population() {
    // Read the positive cached module first; a later retained capture must not consult it.
    let (source, rows) = case(0);
    let cached = Fixture::new(source, rows).await;
    let capture = cached.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(
        collected_resources::cached_plans(&capture).unwrap().report.entries.iter().any(|entry| entry.plan.is_some())
    );
    let (source, rows) = case(1);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    let result = collected_resources::cached_plans(&capture).unwrap();
    let [entry] = result.report.entries.as_slice() else { panic!("one exact retained definition") };
    let plan = entry.plan.as_ref().unwrap();
    assert_eq!(plan.kind, PlanKind::Actual);
    assert_eq!(plan.root.act_rows, Some(10000.0));
    assert_eq!(plan.root.act_executions, Some(1.0));
    assert_eq!(plan.root.facts().relational_input.unwrap().qualified_kind(), RelationalInputKind::Unknown);
    assert_eq!(plan.facts().last_run_at, None);
    assert_eq!(plan.facts().elapsed_ms, None);
    assert!(!entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingActualMeasurements));
    assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnknownRelationalInput));
    assert!(serde_json::to_string(&result.context.records).unwrap().contains("9007199254740993"));
    assert!(!serde_json::to_string(&result.report).unwrap().contains("9007199254740993"));
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn retained_input_completion_survives_authenticated_hydration_and_requires_its_own_native_proof() {
    for evidence in ["complete", "unfinished", "unknown-module", "unavailable-statement"] {
        let (source, mut records) = object_definition_retained_input_evidence();
        replace_fact(&mut records[0], |fact| {
            let ObjectDefinitionFact::MssqlRetainedPlan(native) = fact else { panic!() };
            match evidence {
                "unfinished" => {
                    let DefinitionPayload::Available { text, .. } = &native.batch_plan.payload else { panic!() };
                    native.batch_plan = payload(&text.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"0\""));
                }
                "unknown-module" => native.module_parameters = None,
                "unavailable-statement" => {
                    native.statement.payload =
                        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                }
                _ => {}
            }
        });
        let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition(&records[0]).fact else { panic!() };
        let direct = culprit_mssql::plan_xml::parse_retained_definition(native, &source).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let preview = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        assert!(!preview.is_read_complete());
        assert!(preview.records().is_empty());
        let capture = fixture
            .read_families_with_recovery(
                &[EvidenceFamily::ObjectDefinitions],
                RecoveryPayloadBudget { max_objects: 1, max_bytes: fixture.range.manifest().batches[0].bytes },
            )
            .await;
        assert!(capture.is_read_complete(), "{evidence}: {:?}", capture.gaps());
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), 1);
        assert_eq!(result.context.records[0].row.record, records[0]);
        let [entry] = result.report.entries.as_slice() else { panic!("one retained definition") };
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(plan.kind, PlanKind::Actual);
        assert_eq!(plan.root.facts(), direct.root.facts());
        assert_eq!(plan.root.act_rows, Some(1000.0));
        assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(10_000.0));
        assert_eq!(plan.facts().last_run_at, None);
        assert_eq!(plan.facts().elapsed_ms, None);
        let expected_kind = if matches!(evidence, "unknown-module" | "unavailable-statement") {
            RelationalInputKind::Unknown
        } else {
            RelationalInputKind::TableValuedParameter
        };
        assert_eq!(plan.root.facts().relational_input.unwrap().qualified_kind(), expected_kind, "{evidence}");
        let completion =
            culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "10K");
        let insights = &plan.insights.as_ref().unwrap().items;
        assert_eq!(
            insights.iter().any(|insight| insight.detail.contains(&completion)),
            evidence == "complete",
            "{evidence}"
        );
        assert!(insights.iter().all(|insight| insight.fix_sql.is_none()));
        assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingCurrentPrerequisites));
        assert!(plan.raw.is_none() && plan.statement.is_none());
        let report = serde_json::to_string(&result.report).unwrap();
        assert!(!report.contains("<ShowPlanXML") && !report.contains("SELECT value FROM") && !report.contains("9264"));
        assert!(!report.contains("9007199254740993"));
    }
}

#[tokio::test]
async fn generic_definitions_preserve_unsupported_rows_and_native_counters_without_fake_actual_rows() {
    for index in [2, 3] {
        let (source, records) = case(index);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.report.entries.len(), records.len());
        assert_eq!(result.context.records.len(), records.len());
        for record in records {
            let native = definition(&record);
            let entry = result.report.entries.iter().find(|entry| entry.definition_key == native.key).unwrap();
            assert_eq!(entry.definition_version, native.version);
            match &native.fact {
                ObjectDefinitionFact::RetainedPlan(plan) => {
                    assert_eq!(entry.identity, Some(CachedPlanIdentity::Native(plan.definition.identity.clone())));
                    if source.target.engine() == Engine::Postgres && cfg!(feature = "postgres") {
                        let derived = entry.plan.as_ref().unwrap();
                        assert_eq!(derived.kind, PlanKind::Estimated);
                        assert!(derived.raw.is_none() && derived.statement.is_none());
                        assert_eq!(derived.root.act_rows, None);
                    } else {
                        assert!(entry.plan.is_none());
                        assert!(!entry.limitations.is_empty());
                    }
                }
                _ => {
                    assert!(entry.plan.is_none());
                    assert!(
                        entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::UnsupportedDefinition)
                    );
                }
            }
            let export = ExportedRecord::new(&source, capture.capture(), record).unwrap();
            assert_eq!(result.context.records.iter().filter(|record| record.row == export).count(), 1);
        }
        assert!(!serde_json::to_string(&result.report).unwrap().contains("9007199254740993"));
    }
}

#[tokio::test]
async fn authenticated_yugabyte_node_cache_uses_the_native_parser_and_keeps_independent_counter_scope() {
    let (mut source, records) = case(2);
    let CollectionTarget::Database(database) = &mut source.target else { panic!("selected database") };
    database.engine = Engine::Yugabyte;
    let node = "native-node-1";
    let native_source = "yb_pg_stat_plans";
    source.source = format!("ysql-{native_source}-{node}");
    let SourceObservation::Snapshot(state) = &mut source.observation else { panic!() };
    state.records = 1;
    state.sources = vec![StateSourceCoverage::collected(native_source.into(), 1)];
    let ObjectDefinitionFact::RetainedPlan(mut native) = definition(&records[1]).fact.clone() else { panic!() };
    native.definition.identity = NativePlanIdentity {
        plan_id: "11".into(),
        query: NativeQueryIdentity {
            query_id: "42".into(),
            qualifiers: [
                ("node".into(), node.into()),
                ("database_oid".into(), "7".into()),
                ("userid".into(), "10".into()),
            ]
            .into(),
        },
        qualifiers: [("node".into(), node.into()), ("cache".into(), native_source.into())].into(),
    };
    native.definition.id = native.definition.identity.definition_id(&source).unwrap();
    native.definition.query_definition_id = None;
    native.definition.native_properties = [("cache".into(), native_source.into())].into();
    native.definition.plan = DefinitionPayload::plan(
        include_str!("../../../providers/culprit-yugabyte/tests/data/qpm.json"),
        Engine::Yugabyte,
    );
    native.counters.identity = NativeIdentity::new(&native.definition.id).qualified("node", node);
    native.counters.scope = MeasurementScope::Node;
    native.counters.counters = vec![NativeCounterSample {
        nature: CounterNature::Cumulative,
        ..NativeCounterSample::gauge(
            "executions",
            9_007_199_254_740_993,
            CounterUnit::Count,
            MeasurementScope::Node,
            native_source,
            "calls",
        )
    }];
    for (name, measured) in [("mean_ms", 3.25), ("max_ms", 250.0)] {
        let mut counter =
            NativeCounterSample::gauge(name, 0, CounterUnit::Milliseconds, MeasurementScope::Node, native_source, name);
        counter.value = MeasurementValue::Real(measured);
        native.counters.counters.push(counter);
    }
    native.counters.properties = [("cache-lifetime".into(), "independent".into())].into();
    let expected = native.as_ref().clone();
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new(native_source.into(), ObjectDefinitionFact::RetainedPlan(native)).unwrap(),
    );
    let fixture = Fixture::new(source, vec![record]).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let result = collected_resources::cached_plans(&capture).unwrap();
    let [entry] = result.report.entries.as_slice() else { panic!("one native node-cache definition") };
    assert_eq!(entry.identity, Some(CachedPlanIdentity::Native(expected.definition.identity.clone())));
    if cfg!(feature = "yugabyte") {
        let plan = entry.plan.as_ref().unwrap();
        assert_eq!(plan.engine, Engine::Yugabyte);
        assert_eq!(plan.kind, PlanKind::Estimated);
        assert!(plan.facts().estimated_cost_unavailable);
        assert!(plan.nodes().iter().all(|node| node.facts().estimated_rows_unavailable));
        assert!(plan.root.act_rows.is_none() && plan.facts().elapsed_ms.is_none());
    } else {
        assert!(entry.plan.is_none());
        assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
    }
    let [record] = result.context.records.as_slice() else { panic!() };
    let ObjectDefinitionFact::RetainedPlan(raw) = &definition(&record.row.record).fact else { panic!() };
    assert_eq!(raw.counters, expected.counters);
    assert_eq!(raw.counters.scope, MeasurementScope::Node);
    assert_eq!(raw.counters.identity.qualifiers["node"], node);
    assert_eq!(raw.counters.counters[0].value, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(raw.counters.counters[0].reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(raw.first_used, expected.first_used);
    assert_eq!(raw.last_used, expected.last_used);
    assert!(!serde_json::to_string(&result.report).unwrap().contains("9007199254740993"));
}

#[tokio::test]
async fn native_payload_availability_coverage_and_exact_empty_reads_remain_distinct() {
    for index in [0, 5, 6, 7] {
        let (source, records) = case(index);
        let expected = source.observation.snapshot().unwrap().sources[0].state;
        let expected_len = records.len();
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert_eq!(result.report.sources[0].state, expected);
        assert_eq!(result.report.entries.len(), expected_len);
        assert!(result.context.read_gaps.is_empty());
        if index == 0 {
            assert!(result.report.entries.iter().any(|entry| entry.limitations.iter().any(|limit| {
                limit.reason
                    == CachedPlanLimit::PayloadUnavailable {
                        payload: CachedPlanPayload::EstimatedPlan,
                        reason: PayloadUnavailable::Restricted,
                    }
            })));
            assert!(result.report.entries.iter().any(|entry| entry.limitations.iter().any(|limit| {
                limit.reason
                    == CachedPlanLimit::PayloadUnavailable {
                        payload: CachedPlanPayload::Statement,
                        reason: PayloadUnavailable::Oversized,
                    }
            })));
        }
    }
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn malformed_mismatched_and_ambiguous_xml_remain_native_rows_with_typed_limitations() {
    let (source, rows) = case(0);
    let ObjectDefinitionFact::MssqlCachedDefinition(original) = &definition(&rows[0]).fact else { panic!() };
    let DefinitionPayload::Available { text: xml, .. } = &original.estimated_plan.payload else { panic!() };
    let start = xml.find("<StmtSimple").unwrap();
    let end = xml.find("</StmtSimple>").unwrap() + "</StmtSimple>".len();
    let duplicated = xml.replacen("</Statements>", &format!("{}</Statements>", &xml[start..end]), 1);
    for (xml, expected) in [
        (
            "<ShowPlanXML><broken>".to_owned(),
            CachedPlanLimit::PayloadUnavailable {
                payload: CachedPlanPayload::EstimatedPlan,
                reason: PayloadUnavailable::RedactionFailed,
            },
        ),
        (xml.replace("0x0123456789ABCDEF", "0xFEDCBA9876543210"), CachedPlanLimit::UnusablePlan),
        (duplicated, CachedPlanLimit::UnusablePlan),
    ] {
        let mut rows = rows.clone();
        replace_fact(&mut rows[0], |fact| {
            let ObjectDefinitionFact::MssqlCachedDefinition(native) = fact else { panic!() };
            native.estimated_plan = payload(&xml);
        });
        let key = definition(&rows[0]).key.clone();
        let fixture = Fixture::new(source.clone(), rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        let result = collected_resources::cached_plans(&capture).unwrap();
        let entry = result.report.entries.iter().find(|entry| entry.definition_key == key).unwrap();
        assert!(entry.plan.is_none());
        assert!(entry.identity.is_some());
        assert!(entry.limitations.iter().any(|limit| limit.reason == expected), "{:?}", entry.limitations);
        assert_eq!(result.context.records.len(), 3);
    }
}

#[tokio::test]
async fn missing_rows_and_publication_are_read_gaps_without_a_healthy_plan_verdict() {
    for unpublished in [false, true] {
        let (source, rows) = case(0);
        let fixture = Fixture::new(source, rows).await;
        if unpublished {
            fixture.unpublish();
        } else {
            fixture.remove_first_row(TelemetryStream::ObjectDefinitions);
        }
        let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
        let result = collected_resources::cached_plans(&capture).unwrap();
        assert!(!result.context.read_gaps.is_empty());
        assert_eq!(result.report.sources[0].state, SourceState::Partial);
        assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
        if unpublished {
            assert!(result.report.entries.is_empty());
            assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::NotPublished));
        }
    }
}

#[tokio::test]
async fn wrong_dataset_subject_and_foreign_generic_definition_scope_are_rejected() {
    let (source, rows) = case(6);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::cached_plans(&capture).is_err(), "object definitions were not selected");

    let (mut source, rows) = case(0);
    let SourceObservation::Snapshot(state) = &mut source.observation else { panic!() };
    state.dataset = SnapshotDataset::Objects;
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(collected_resources::cached_plans(&capture).is_err());

    let (mut source, rows) = case(6);
    source.target = memory_failure_evidence().0.target;
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(collected_resources::cached_plans(&capture).is_err());

    let (source, mut rows) = case(2);
    replace_fact(&mut rows[1], |fact| {
        let ObjectDefinitionFact::RetainedPlan(native) = fact else { panic!() };
        native.definition.id = "b".repeat(64);
        native.counters.identity.id = native.definition.id.clone();
    });
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(collected_resources::cached_plans(&capture).is_err());
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn missing_current_metadata_withholds_optimizer_requested_index_sql_without_erasing_advice() {
    let (source, mut rows) = case(0);
    replace_fact(&mut rows[0], |fact| {
        let ObjectDefinitionFact::MssqlCachedDefinition(native) = fact else { panic!() };
        let DefinitionPayload::Available { text, .. } = &native.estimated_plan.payload else { panic!() };
        let missing = r#"<MissingIndexes><MissingIndexGroup Impact="99"><MissingIndex Database="[db]" Schema="[dbo]" Table="[items]"><ColumnGroup Usage="EQUALITY"><Column Name="[value]" /></ColumnGroup></MissingIndex></MissingIndexGroup></MissingIndexes>"#;
        native.estimated_plan = payload(&text.replace("</QueryPlan>", &format!("{missing}</QueryPlan>")));
    });
    let ObjectDefinitionFact::MssqlCachedDefinition(native) = &definition(&rows[0]).fact else { panic!() };
    let key = definition(&rows[0]).key.clone();
    let direct = culprit_mssql::plan_xml::parse_cached_definition(native, &source).unwrap();
    assert!(culprit_core::insights::insights(&direct).iter().any(|insight| insight.fix_sql.is_some()));
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    let result = collected_resources::cached_plans(&capture).unwrap();
    let entry = result.report.entries.iter().find(|entry| entry.definition_key == key).unwrap();
    let insights = &entry.plan.as_ref().unwrap().insights.as_ref().unwrap().items;
    assert!(insights.iter().any(|insight| insight.rule == "missing-index-hint"));
    assert!(insights.iter().all(|insight| insight.fix_sql.is_none()));
    assert!(insights.iter().any(|insight| !insight.fix.is_empty()));
    assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::MissingCurrentPrerequisites));
}

#[cfg(not(feature = "mssql"))]
#[tokio::test]
async fn a_build_without_the_native_parser_keeps_exact_rows_and_explicit_unavailability() {
    let (source, rows) = case(0);
    let key = definition(&rows[0]).key.clone();
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    let result = collected_resources::cached_plans(&capture).unwrap();
    let entry = result.report.entries.iter().find(|entry| entry.definition_key == key).unwrap();
    assert!(entry.plan.is_none());
    assert!(entry.limitations.iter().any(|limit| limit.reason == CachedPlanLimit::NativeParserUnavailable));
    assert_eq!(result.context.records.len(), 3);
}
