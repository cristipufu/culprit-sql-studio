use super::*;
use culprit_core::{
    Plan,
    telemetry::{MssqlModuleParameters, NativeReadContext},
};

const SQL: &str = "SELECT r.id FROM @Rows r JOIN dbo.Items i ON r.id=i.id";
const ROWS: &str = r#"Table="[@Rows]" Alias="[r]""#;
const ITEMS: &str = r#"Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]""#;

fn scan(id: u32, object: &str) -> String {
    format!(
        r#"<RelOp NodeId="{id}" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="0" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="20000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation>
<TableScan><Object {object} /></TableScan></RelOp>"#
    )
}

fn join(sql: &str, left: &str, right: &str, declarations: &str) -> String {
    document(&format!(
        r#"{declarations}<StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="3"><QueryPlan>
<RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" Parallel="0" EstimateRows="100" EstimatedTotalSubtreeCost="3">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1" /></RunTimeInformation>
<NestedLoops>{left}{right}</NestedLoops></RelOp></QueryPlan></StmtSimple>"#
    ))
}

fn parse(sql: &str, xml: &str, read: Option<&NativeReadContext>) -> Plan {
    let mut statement = crate::cached_statements::context(&crate::grants::tests::row()).unwrap();
    statement.query_hash = Some("0x0123456789ABCDEF".into());
    let parameters = MssqlModuleParameters {
        database_id: statement.database_id,
        object_id: 1001,
        module_number: 1,
        sql_handle: statement.identity.sql_handle.clone(),
        table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
    };
    crate::plan_xml::parse_cached_statement(
        xml,
        crate::plan_xml::CachedInputContext {
            statement: &statement,
            module_parameters: Some(&parameters),
            text: Some(sql),
            origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read },
        },
    )
    .unwrap()
}

fn kinds(plan: &Plan) -> Vec<Kind> {
    plan.root
        .children
        .iter()
        .filter_map(|node| node.facts.relational_input.map(|input| input.qualified_kind()))
        .collect()
}

#[test]
fn original_module_and_captured_database_bind_a_tvp_beside_an_exact_named_join_input() {
    let xml = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(SQL, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    let input = &plan.root.children[0];
    assert_eq!(input.facts.relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(input.facts.access_rows.unwrap().actual_total, Some(20000.0));
    assert_eq!(input.act_rows, Some(10000.0));
    assert!(input.facts.scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    let insights = culprit_core::insights::insights(&plan);
    assert!(insights.iter().any(|insight| insight.rule == "misestimate"));
    assert!(
        insights.iter().all(|insight| !insight.detail.contains(&culprit_core::t!(
            "insight.relational_input.completed_input_counts",
            estimated = "100",
            actual = "20K"
        ))),
        "a completed scan under JOIN does not establish the entire TVP input population"
    );
    assert_eq!(kinds(&parse(SQL, &xml, None)), vec![Kind::Unknown]);
    assert_eq!(kinds(&parse(SQL, &xml, Some(&crate::plan_xml::test_read_context("other")))), vec![Kind::Unknown]);
}

#[test]
fn closed_all_variable_join_preserves_module_and_local_proofs_without_a_reader_namespace() {
    let sql = "SELECT r.id FROM @Rows r JOIN @Local l ON r.id=l.id";
    let local = r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#;
    let xml = join(sql, &scan(1, ROWS), &scan(2, r#"Table="[@Local]" Alias="[l]""#), local);
    assert_eq!(kinds(&parse(sql, &xml, None)), vec![Kind::TableValuedParameter, Kind::LocalTableVariable]);
    let unknown = xml.replace("DECLARE @Local TABLE(id int)", "DECLARE @Local dbo.RowsType");
    assert_eq!(kinds(&parse(sql, &unknown, None)), vec![Kind::TableValuedParameter, Kind::Unknown]);
    let ambiguous = join(sql, &scan(1, ROWS), &scan(2, r#"Table="[@Local]" Alias="[l]""#), &local.repeat(2));
    assert_eq!(kinds(&parse(sql, &ambiguous, None)), vec![Kind::TableValuedParameter, Kind::Unknown]);
}

#[test]
fn explicit_cross_database_names_match_native_objects_without_guessing_default_schema() {
    let read = crate::plan_xml::test_read_context("app");
    let sql = SQL.replace("dbo.Items", "[History].[archive].[Items]");
    let object = ITEMS.replace("[app]", "[History]").replace("[dbo]", "[archive]");
    let xml = join(&sql, &scan(1, ROWS), &scan(2, &object), "");
    assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::TableValuedParameter]);
    assert_eq!(kinds(&parse(&sql, &xml, None)), vec![Kind::Unknown]);
    let wrong = xml.replace("Database=\"[History]\"", "Database=\"[Other]\"");
    assert_eq!(kinds(&parse(&sql, &wrong, Some(&read))), vec![Kind::Unknown]);
    for unresolved in ["Items", "app..Items"] {
        let sql = SQL.replace("dbo.Items", unresolved);
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{unresolved}");
    }
}

#[test]
fn exact_quoted_table_and_alias_spelling_is_shared_with_native_object_decoding() {
    let sql = "SELECT r.id FROM @Rows r JOIN [s]]chema].[I.tems] [i]]tem] ON r.id=[i]]tem].id";
    let object = r#"Database="[app]" Schema="[s]]chema]" Table="[I.tems]" Alias="[i]]tem]""#;
    let read = crate::plan_xml::test_read_context("app");
    let xml = join(sql, &scan(1, ROWS), &scan(2, object), "");
    assert_eq!(kinds(&parse(sql, &xml, Some(&read))), vec![Kind::TableValuedParameter]);
    let wrong_case = xml.replace("Alias=\"[r]\"", "Alias=\"[R]\"");
    assert_eq!(kinds(&parse(sql, &wrong_case, Some(&read))), vec![Kind::Unknown]);
}

#[test]
fn expanded_missing_extra_or_ambiguous_objects_cannot_borrow_an_outer_parameter_kind() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    for xml in [
        base.replace("Table=\"[Items]\"", "Table=\"[ExpandedBase]\""),
        base.replace("Alias=\"[i]\"", "Alias=\"[expanded]\""),
        base.replace("Schema=\"[dbo]\"", "Schema=\"[other]\""),
        base.replace("Database=\"[app]\"", ""),
        base.replace("NodeId=\"2\"", "NodeId=\"1\""),
        base.replace("NodeId=\"2\"", ""),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, r#"Table="[@Other]" Alias="[x]""#))),
        join(SQL, &scan(1, ROWS), "", ""),
        base.replace("<Object Database=", "<Object Server=\"[linked]\" Database="),
    ] {
        let plan = parse(SQL, &xml, Some(&read));
        assert_eq!(plan.root.children[0].facts.relational_input.unwrap().qualified_kind(), Kind::Unknown, "{xml}");
    }
}

#[test]
fn healthy_cardinality_and_competing_native_tvf_keep_their_own_evidence() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let healthy = base
        .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
    let plan = parse(SQL, &healthy, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
    let sql = SQL.replace("dbo.Items i", "dbo.Items(r.id) i");
    let tvf = scan(2, ITEMS)
        .replace("<TableScan>", "<TableValuedFunction>")
        .replace("</TableScan>", "</TableValuedFunction>");
    let plan = parse(&sql, &join(&sql, &scan(1, ROWS), &tvf, ""), Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::Unknown, Kind::TableValuedFunction]);
}

#[test]
fn a_proven_tvp_does_not_suppress_native_cartesian_evidence_or_establish_full_input_size() {
    let read = crate::plan_xml::test_read_context("app");
    let sql = "SELECT r.id FROM @Rows r CROSS JOIN dbo.Items i";
    let xml = join(sql, &scan(1, ROWS), &scan(2, ITEMS), "")
        .replace("<NestedLoops>", "<Warnings NoJoinPredicate=\"true\" /><NestedLoops>");
    let plan = parse(sql, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(plan.root.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate));
    let insights = culprit_core::insights::insights(&plan);
    assert!(insights.iter().any(|insight| insight.rule == "no-join-predicate"));
    let full_input =
        culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K");
    assert!(insights.iter().all(|insight| !insight.detail.contains(&full_input)));
}

#[test]
fn pre_redaction_bindings_restore_cached_and_retained_named_joins_without_current_namespace_inference() {
    use culprit_core::telemetry::*;
    let sql = "SELECT r.id FROM @Rows r CROSS JOIN dbo.Items i";
    let raw = join(sql, &scan(1, ROWS), &scan(2, ITEMS), "")
        .replace("<NestedLoops>", "<Warnings NoJoinPredicate=\"true\" /><NestedLoops>");
    for retained in [false, true] {
        let xml = if retained {
            raw.clone()
        } else {
            raw.lines().filter(|line| !line.contains("<RunTimeInformation>")).collect::<Vec<_>>().join("\n")
        };
        let row = crate::cached_definitions::tests::module_row(sql, &xml);
        let source = crate::plan_xml::test_source(if retained {
            SnapshotDataset::RetainedPlans
        } else {
            SnapshotDataset::CachedDefinitions
        });
        let (plan, proof, without) = if retained {
            let layout = crate::retained_definitions::Layout {
                maximum_bytes: 32 * 1024,
                maximum_statement_bytes: 4096,
                database_setting: Some(true),
            };
            let native = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
            let proof = native.relational_input_proof.clone().expect("original namespace is retained before redaction");
            let plan = crate::plan_xml::parse_retained_definition(&native, &source).unwrap();
            let mut missing = native.clone();
            missing.relational_input_proof = None;
            (plan, proof, crate::plan_xml::parse_retained_definition(&missing, &source).unwrap())
        } else {
            let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
            let native = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
            let proof = native.relational_input_proof.clone().expect("original namespace is retained before redaction");
            let plan = crate::plan_xml::parse_cached_definition(&native, &source).unwrap();
            let mut missing = native.clone();
            missing.relational_input_proof = None;
            (plan, proof, crate::plan_xml::parse_cached_definition(&missing, &source).unwrap())
        };
        assert_eq!(proof.bindings.len(), 1);
        assert_eq!(proof.bindings[0].node_id, 1);
        assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
        assert_eq!(kinds(&without), vec![Kind::Unknown]);
        assert_eq!(
            plan.kind,
            if retained { culprit_core::PlanKind::Actual } else { culprit_core::PlanKind::Estimated }
        );
        let insights = culprit_core::insights::insights(&plan);
        assert!(insights.iter().any(|insight| insight.rule == "no-join-predicate"));
        assert!(insights.iter().all(|insight| !insight.detail.contains(&culprit_core::t!(
            "insight.relational_input.completed_input_counts",
            estimated = "100",
            actual = "20K"
        ))));
        let encoded = serde_json::to_string(&proof).unwrap();
        assert!(!encoded.contains("app") && !encoded.contains("Items") && !encoded.contains("RowsType"));
    }
}

#[test]
fn present_binding_for_a_different_source_or_native_node_is_an_integrity_failure() {
    use culprit_core::telemetry::*;
    let source = crate::plan_xml::test_source(SnapshotDataset::CachedDefinitions);
    let row = crate::cached_definitions::tests::module_row(SQL, &join(SQL, &scan(1, ROWS), &scan(2, ITEMS), ""));
    let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    let original = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
    assert!(original.relational_input_proof.is_some());
    let mut other = source.clone();
    other.source = "another-capture-source".into();
    for (value, scope) in [(&original, &other), (&original, &source)] {
        let mut value = value.clone();
        if scope == &source {
            value.relational_input_proof = None;
            value.relational_input_proof = Some(
                MssqlRelationalInputProof::for_cached(
                    scope,
                    &value,
                    vec![MssqlRelationalInputBinding {
                        node_id: 999,
                        input: RelationalInput {
                            kind: Kind::TableValuedParameter,
                            provenance: Provenance::NativeParameterMetadata,
                        },
                    }],
                )
                .unwrap(),
            );
            assert!(value.is_valid(), "the native parser must reject a self-consistent but false NodeId claim");
        }
        let error = crate::plan_xml::parse_cached_definition(&value, scope).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[test]
fn unsupported_namespace_scope_keeps_payloads_and_does_not_mint_a_retained_binding() {
    use culprit_core::telemetry::*;
    let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    for scenario in ["foreign-object", "missing-member", "missing-epoch", "nested-function", "missing-module"] {
        let mut source = crate::plan_xml::test_source(SnapshotDataset::CachedDefinitions);
        let mut xml = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
        if scenario == "foreign-object" {
            xml = xml.replace("Database=\"[app]\"", "Database=\"[other]\"");
        }
        if scenario == "nested-function" {
            xml = xml.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">");
        }
        if scenario == "missing-member" {
            let SourcePartition::InstanceLocal { member, .. } = &mut source.partition else { panic!() };
            *member = None;
        }
        if scenario == "missing-epoch" {
            let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
            *epoch = None;
        }
        let mut row = crate::cached_definitions::tests::module_row(SQL, &xml);
        if scenario == "missing-module" {
            row = crate::resources::test_support::replace(&row, &[("module_object_id", crate::value::Val::Null)]);
        }
        let value = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
        assert!(value.relational_input_proof.is_none(), "{scenario}");
        assert!(value.statement.payload.is_available() && value.estimated_plan.payload.is_available());
    }
}
