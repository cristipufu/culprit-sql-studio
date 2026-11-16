use super::*;
use culprit_core::{Engine, PlanKind, plan_facts::RelationalInputKind};

const HASH: &str = "0x0123456789ABCDEF";
const SQL: &str = "SELECT * FROM @Rows";

fn context() -> CachedStatementContext {
    CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x0102".into(),
            sql_handle: "0x0304".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2026-11-14T12:00:00.003".into(),
            plan_generation: "2".into(),
        },
        database_id: 7,
        query_hash: Some(HASH.into()),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: Some((SQL.len() * 2).to_string()),
        source_text_complete: true,
    }
}

fn statement(hash: &str, cost: u32, actual: bool) -> String {
    let runtime = if actual {
        r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation>"#
    } else {
        ""
    };
    format!(
        r#"<StmtSimple StatementText="{SQL}" QueryHash="{hash}" StatementSubTreeCost="{cost}"><QueryPlan>
        <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="{cost}">
        {runtime}<TableScan><Object Table="[@Rows]" /></TableScan></RelOp>
        <QueryTimeStats ElapsedTime="0" CpuTime="0" /></QueryPlan></StmtSimple>"#
    )
}

fn document(statements: &str) -> String {
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn payload(raw: &str, redact: fn(&str, Engine) -> DefinitionPayload) -> MssqlCachedPayload {
    crate::cached_payload::from_raw(
        crate::query_store::payload::Payload::Text(raw.into()),
        Some(raw.encode_utf16().count() * 2),
        32 * 1024,
        redact,
    )
    .unwrap()
}

fn cached(xml: &str) -> MssqlCachedDefinition {
    let context = context();
    MssqlCachedDefinition {
        relational_input_proof: None,
        query_plan_hash: None,
        statement: payload(SQL, DefinitionPayload::sql),
        estimated_plan: payload(xml, DefinitionPayload::plan),
        module_parameters: Some(MssqlModuleParameters {
            database_id: context.database_id,
            object_id: 1001,
            module_number: 1,
            sql_handle: context.identity.sql_handle.clone(),
            table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
        }),
        context,
    }
}

#[test]
fn native_statement_selection_does_not_substitute_the_heaviest_plan() {
    let xml = document(&format!("{}{}", statement(HASH, 1, false), statement("0xFEDCBA9876543210", 99, false)));
    let selected = parse_cached_definition(
        &cached(&xml),
        &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions),
    )
    .unwrap();
    assert_eq!(selected.total_cost, 1.0);
    assert_eq!(selected.facts().query_hash.as_deref(), Some(HASH));
    assert_eq!(super::super::parse(&xml).unwrap().total_cost, 99.0, "pasted-plan behavior is independent");
}

#[test]
fn missing_and_duplicate_native_hash_matches_cannot_become_another_statement() {
    for statements in [
        statement("0xFEDCBA9876543210", 99, false),
        format!("{}{}", statement(HASH, 1, false), statement(HASH, 99, false)),
    ] {
        let xml = document(&statements);
        assert!(
            parse_cached_definition(
                &cached(&xml),
                &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
            )
            .is_err()
        );
        assert!(super::super::parse(&xml).is_ok(), "a pasted batch remains independently inspectable");
    }
}

#[test]
fn a_missing_hash_requires_one_statement_and_nonzero_native_tokens() {
    let mut definition = cached(&document(&statement(HASH, 1, false)));
    definition.context.query_hash = None;
    assert!(
        parse_cached_definition(
            &definition,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
        )
        .is_ok()
    );
    definition.estimated_plan = payload(
        &document(&format!("{}{}", statement(HASH, 1, false), statement("0xFEDCBA9876543210", 99, false))),
        DefinitionPayload::plan,
    );
    assert!(
        parse_cached_definition(
            &definition,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
        )
        .is_err()
    );
    for zero in ["plan", "sql"] {
        let mut definition = cached(&document(&statement(HASH, 1, false)));
        if zero == "plan" {
            definition.context.identity.plan_handle = "0x0000".into();
        } else {
            definition.context.identity.sql_handle = "0x0000".into();
            definition.module_parameters = None;
        }
        assert!(
            parse_cached_definition(
                &definition,
                &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
            )
            .is_err()
        );
    }
}

#[test]
fn cached_module_classification_does_not_manufacture_actual_rows_or_a_tvp_problem() {
    let definition = cached(&document(&statement(HASH, 1, false)));
    let plan = parse_cached_definition(
        &definition,
        &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions),
    )
    .unwrap();
    assert_eq!(plan.root.facts().relational_input.unwrap().qualified_kind(), RelationalInputKind::TableValuedParameter);
    assert_eq!(plan.kind, PlanKind::Estimated);
    assert_eq!(plan.root.est_rows, 100.0);
    assert_eq!(plan.root.act_rows, None);
    assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
    let mut foreign = definition;
    foreign.module_parameters.as_mut().unwrap().sql_handle = "0x0506".into();
    assert!(
        parse_cached_definition(
            &foreign,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
        )
        .is_err()
    );
}

#[test]
fn retained_actual_rows_have_no_borrowed_module_kind_cache_population_or_placeholder_time() {
    let definition = MssqlRetainedPlanDefinition {
        relational_input_proof: None,
        context: context(),
        statement_executions: Some(u64::MAX.to_string()),
        last_start_without_offset: Some("2026-11-14T11:59:59.997".into()),
        database_last_query_plan_stats: Some(false),
        statement: MssqlCachedPayload {
            source_utf16_bytes: None,
            native_sha256: None,
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        },
        module_parameters: None,
        batch_plan: payload(&document(&statement(HASH, 1, true)), DefinitionPayload::plan),
    };
    let plan = parse_retained_definition(
        &definition,
        &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans),
    )
    .unwrap();
    assert_eq!(plan.kind, PlanKind::Actual);
    assert_eq!(plan.root.act_rows, Some(10000.0));
    assert_eq!(plan.root.act_executions, Some(1.0));
    assert_eq!(plan.root.facts().relational_input.unwrap().qualified_kind(), RelationalInputKind::Unknown);
    assert_eq!(plan.facts().elapsed_ms, None);
    assert_eq!(plan.facts().cpu_ms, None);
    assert_eq!(plan.facts().last_run_at, None);
    assert!(!plan.props.contains_key("CachedExecutions"));
}

#[test]
fn unavailable_and_malformed_native_plans_remain_unusable() {
    let mut definition = cached(&document(&statement(HASH, 1, false)));
    definition.estimated_plan.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    assert!(
        parse_cached_definition(
            &definition,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
        )
        .is_err()
    );
    definition.estimated_plan = payload("<ShowPlanXML><broken>", DefinitionPayload::plan);
    assert!(
        parse_cached_definition(
            &definition,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::CachedDefinitions)
        )
        .is_err()
    );
    let retained = MssqlRetainedPlanDefinition {
        relational_input_proof: None,
        context: context(),
        statement_executions: Some("0".into()),
        last_start_without_offset: None,
        database_last_query_plan_stats: None,
        statement: MssqlCachedPayload {
            source_utf16_bytes: None,
            native_sha256: None,
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        },
        module_parameters: None,
        batch_plan: definition.estimated_plan,
    };
    assert!(
        parse_retained_definition(
            &retained,
            &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans)
        )
        .is_err()
    );
}

#[test]
fn raw_native_namespace_requires_valid_database_read_context() {
    let definition = cached(&document(&statement(HASH, 1, false)));
    let mut reversed = crate::plan_xml::test_read_context("app");
    reversed.observed_until = "2026-11-14T11:59:59Z".into();
    let mut shared = crate::plan_xml::test_read_context("app");
    shared.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("controlled-fixture"),
        native: NativeIdentity::new("instance"),
    });
    shared.partition = SourcePartition::Target;
    assert!(shared.is_valid());
    let mut foreign = crate::plan_xml::test_read_context("app");
    let CollectionTarget::Database(database) = &mut foreign.target else { panic!() };
    database.engine = Engine::Postgres;
    let DefinitionPayload::Available { text, .. } = &definition.estimated_plan.payload else { panic!() };
    for read in [reversed, shared, foreign] {
        assert!(
            super::super::parse_cached_statement(
                text,
                super::super::CachedInputContext {
                    statement: &definition.context,
                    module_parameters: definition.module_parameters.as_ref(),
                    text: Some(SQL),
                    origin: super::super::PlanInputOrigin::OriginalNative { read: Some(&read) },
                },
            )
            .is_err()
        );
    }
}
