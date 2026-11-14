use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    plan_facts::RelationalInputKind,
    telemetry::{CachedStatementIdentity, StatementOrigin},
};

const HEADER: &str = "CREATE PROC dbo.consume @rows dbo.RowsType READONLY AS SELECT * FROM @rows";
fn context() -> CachedStatementContext {
    CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x0200".into(),
            sql_handle: "0x0300".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2026-11-14T12:00:00.123".into(),
            plan_generation: "1".into(),
        },
        database_id: 7,
        query_hash: Some("0x0123456789ABCDEF".into()),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: Some((HEADER.len() * 2).to_string()),
        source_text_complete: true,
    }
}
fn header_row(header: &str) -> Rec {
    Rec::of(&[
        ("module_database_id", Val::Int(7)),
        ("module_object_id", Val::Int(1001)),
        ("module_number", Val::Int(1)),
        ("encrypted", Val::Bool(false)),
        ("text_prefix", Val::Text(header.into())),
    ])
}
pub(crate) fn binding_row(context: &CachedStatementContext) -> Rec {
    Rec::of(&[
        ("plan_handle", Val::Text(context.identity.plan_handle.clone())),
        ("sql_handle", Val::Text(context.identity.sql_handle.clone())),
    ])
}

#[test]
fn original_header_readonly_parameter_and_same_type_local_remain_distinct_without_serializing_defaults() {
    for (header, known) in [
        (HEADER, true),
        ("CREATE PROC dbo.consume @n int AS DECLARE @rows dbo.RowsType; SELECT * FROM @rows", false),
        (
            "CREATE PROC dbo.consume @secret nvarchar(100)='private-value',@rows dbo.RowsType READONLY AS SELECT * FROM @rows",
            true,
        ),
    ] {
        let mut context = context();
        context.source_text_bytes = Some((header.encode_utf16().count() * 2).to_string());
        let parameters = parameters(&header_row(header), &context).unwrap().unwrap();
        assert_eq!(parameters.input(&context, "@rows").is_some(), known);
        assert!(parameters.input(&context, "@Rows").is_none(), "native spelling is not universally case-folded");
        let serialized = serde_json::to_string(&parameters).unwrap();
        assert!(!serialized.contains("private-value"));
        assert!(!serialized.contains("SELECT"));
    }
}

#[tokio::test]
async fn one_independent_batched_attribute_lookup_confirms_only_the_original_sql_token() {
    let first = context();
    let mut second = first.clone();
    second.identity.plan_handle = "0x0201".into();
    let mut bindings = Bindings::default();
    assert!(bindings.admit(&first).unwrap());
    assert!(bindings.admit(&second).unwrap());
    let mut reader = FakeRead::new(vec![Ok(vec![binding_row(&second), binding_row(&first)])]);
    let verified = bindings.verify(&mut reader).await.unwrap();
    assert!(bindings.all_verified(&verified));
    assert!(verified.contains(&first));
    assert_eq!(reader.calls.len(), 1);
    let call = &reader.calls[0];
    assert!(call.sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
    for prohibited in ["sys.parameters", "sys.sql_modules", "sys.dm_exec_query_stats", "GROUP BY", "ORDER BY"] {
        assert!(!call.sql.contains(prohibited), "{prohibited}");
    }
    assert!(!call.sql.contains("0x0200"));
    let Parameter::Text(selectors) = &call.parameters[0] else { panic!("bound handles") };
    assert!(selectors.contains("0x0200") && selectors.contains("0x0201"));
}

#[tokio::test]
async fn reused_plan_handle_after_identical_body_alter_and_missing_evicted_handles_are_not_a_match() {
    let original = context();
    let mut bindings = Bindings::default();
    bindings.admit(&original).unwrap();
    for sql in [Val::Text("0x0301".into()), Val::Null] {
        let changed = replace(&binding_row(&original), &[("sql_handle", sql)]);
        let mut reader = FakeRead::new(vec![Ok(vec![changed])]);
        let verified = bindings.verify(&mut reader).await.unwrap();
        assert!(!verified.contains(&original));
        assert!(!bindings.all_verified(&verified));
    }
    let mut changed = original.clone();
    changed.identity.sql_handle = "0x0301".into();
    assert_eq!(CollectionStop::from_error(&bindings.admit(&changed).unwrap_err()), CollectionStop::SourceChanged);
}

#[tokio::test]
async fn same_original_sql_header_after_recompile_is_not_a_runtime_counter_generation_proof() {
    let original = context();
    let mut bindings = Bindings::default();
    bindings.admit(&original).unwrap();
    let mut reader = FakeRead::new(vec![Ok(vec![binding_row(&original)])]);
    let verified = bindings.verify(&mut reader).await.unwrap();
    let mut recompiled = original.clone();
    recompiled.identity.created_without_offset = "2026-11-14T12:00:01".into();
    recompiled.identity.plan_generation = "2".into();
    assert!(verified.contains(&recompiled), "the original module token can establish only declaration kind");
    assert!(!bindings.contains(&recompiled), "this set never observed the new counter identity");
}

#[test]
fn hidden_foreign_encrypted_truncated_dynamic_or_unqualified_headers_do_not_become_no_tvp() {
    let context = context();
    let base = header_row(HEADER);
    for row in [
        replace(&base, &[("module_object_id", Val::Null)]),
        replace(&base, &[("module_object_id", Val::Int(-1001))]),
        replace(&base, &[("module_database_id", Val::Int(8))]),
        replace(&base, &[("module_number", Val::Int(2))]),
        replace(&base, &[("encrypted", Val::Bool(true))]),
        replace(&base, &[("text_prefix", Val::Null)]),
    ] {
        assert!(parameters(&row, &context).unwrap().is_none());
    }
    for prefix in [
        "CREATE PROC dbo.consume @rows dbo.RowsType READONLY",
        "EXEC(N'CREATE PROC dbo.consume @rows dbo.RowsType READONLY AS SELECT 1')",
        "DECLARE @rows dbo.RowsType",
    ] {
        let mut context = context.clone();
        context.source_text_complete = false;
        context.source_text_bytes = Some("4096".into());
        assert!(parameters(&header_row(prefix), &context).unwrap().is_none());
    }
}

#[tokio::test]
async fn native_cancellation_budget_and_permission_errors_preserve_their_cause_without_retry() {
    let context = context();
    let mut bindings = Bindings::default();
    bindings.admit(&context).unwrap();
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::ByteLimit,
    ] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let error = bindings.verify(&mut reader).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls.len(), 1);
    }
    let mut reader = FakeRead::new(vec![Err(ReadError::Source(culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        "native permission denied",
    )))]);
    assert_eq!(bindings.verify(&mut reader).await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn duplicate_unexpected_missing_and_overflowed_native_bindings_are_rejected() {
    let context = context();
    let mut bindings = Bindings::default();
    bindings.admit(&context).unwrap();
    for rows in [
        vec![],
        vec![binding_row(&context), binding_row(&context)],
        vec![replace(&binding_row(&context), &[("plan_handle", Val::Text("0x9999".into()))])],
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(bindings.verify(&mut reader).await.is_err());
    }
    let mut empty = FakeRead::new(vec![]);
    assert!(Bindings::default().verify(&mut empty).await.unwrap().pairs.is_empty());
    assert!(empty.calls.is_empty());
    let mut capped = Bindings::default();
    for value in 1..=MAX_BINDINGS {
        let mut context = context.clone();
        context.identity.plan_handle = format!("0x{value:04x}");
        assert!(capped.admit(&context).unwrap());
    }
    let mut extra = context.clone();
    extra.identity.plan_handle = "0xffff".into();
    assert!(!capped.admit(&extra).unwrap());
    assert!(!capped.contains(&extra));
    let mut generations = Bindings::default();
    for value in 1..=MAX_BINDINGS {
        let mut recompiled = context.clone();
        recompiled.identity.plan_generation = value.to_string();
        assert!(generations.admit(&recompiled).unwrap());
    }
    let mut unadmitted = context.clone();
    unadmitted.identity.plan_generation = (MAX_BINDINGS + 1).to_string();
    assert!(!generations.admit(&unadmitted).unwrap(), "reused handles cannot bypass the witness-state bound");
    assert_eq!(generations.pairs.len(), 1);
}

const VARIABLE_PLAN: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT * FROM @rows" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1"><TableScan><Object Table="[@rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
fn parsed_input(
    xml: &str,
    context: &CachedStatementContext,
    parameters: &MssqlModuleParameters,
    statement: &str,
) -> RelationalInputKind {
    crate::plan_xml::parse_cached_statement(xml, context, Some(parameters), Some(statement))
        .unwrap()
        .root
        .facts
        .relational_input
        .unwrap()
        .qualified_kind()
}

#[test]
fn annotation_requires_the_selected_statement_hash_and_exact_native_parameter_spelling() {
    let context = context();
    let parameters = parameters(&header_row(HEADER), &context).unwrap().unwrap();
    assert_eq!(
        parsed_input(VARIABLE_PLAN, &context, &parameters, "SELECT * FROM @rows"),
        RelationalInputKind::TableValuedParameter
    );
    assert_eq!(
        parsed_input(VARIABLE_PLAN, &context, &parameters, "SELECT * FROM @other"),
        RelationalInputKind::Unknown
    );
    let case = VARIABLE_PLAN.replace("Table=\"[@rows]\"", "Table=\"[@Rows]\"");
    assert_eq!(parsed_input(&case, &context, &parameters, "SELECT * FROM @rows"), RelationalInputKind::Unknown);
    let mut changed = context.clone();
    changed.query_hash = Some("0x0000000000000001".into());
    assert_eq!(parsed_input(VARIABLE_PLAN, &changed, &parameters, "SELECT * FROM @rows"), RelationalInputKind::Unknown);
}

#[test]
fn same_spelling_in_expanded_or_internal_scopes_does_not_borrow_the_outer_modules_parameter_kind() {
    let context = context();
    let parameters = parameters(&header_row(HEADER), &context).unwrap().unwrap();
    for statement in [
        "SELECT * FROM dbo.expand(1)",
        "SELECT dbo.expand(id) FROM @rows",
        "SELECT r.id FROM @rows r JOIN dbo.visible_view v ON v.id=r.id",
    ] {
        let xml = VARIABLE_PLAN.replace("SELECT * FROM @rows", statement);
        assert_eq!(parsed_input(&xml, &context, &parameters, statement), RelationalInputKind::Unknown, "{statement}");
    }
    let inline = VARIABLE_PLAN.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">");
    assert_eq!(
        parsed_input(&inline, &context, &parameters, "SELECT * FROM @rows"),
        RelationalInputKind::Unknown,
        "native inlining cannot be authorized by an outer token occurrence"
    );
    let nested=VARIABLE_PLAN.replace("</TableScan>",r#"<InternalInfo><RelOp NodeId="8" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1" EstimatedTotalSubtreeCost="0"><TableScan><Object Table="[@rows]" /></TableScan></RelOp></InternalInfo></TableScan>"#);
    let plan =
        crate::plan_xml::parse_cached_statement(&nested, &context, Some(&parameters), Some("SELECT * FROM @rows"))
            .unwrap();
    assert_eq!(plan.root.facts.relational_input.unwrap().qualified_kind(), RelationalInputKind::Unknown);
    assert_eq!(plan.root.children[0].facts.relational_input.unwrap().qualified_kind(), RelationalInputKind::Unknown);
    let udf=VARIABLE_PLAN.replace("</StmtSimple>",r#"<UDF ProcName="dbo.expanded"><Statements><StmtSimple StatementText="SELECT * FROM @rows"><QueryPlan><RelOp NodeId="9" PhysicalOp="Table Scan" EstimateRows="1"><TableScan><Object Table="[@rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></UDF></StmtSimple>"#);
    assert_eq!(parsed_input(&udf, &context, &parameters, "SELECT * FROM @rows"), RelationalInputKind::Unknown);
    assert!(
        parameters.input(&context, "@rows").is_some(),
        "raw original declaration metadata remains useful when node scope is unknown"
    );
}
