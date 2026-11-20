use super::*;

const SQL: &str = "SELECT d.input_id FROM @Rows AS r CROSS APPLY (SELECT r.id AS input_id) AS d";

fn native(sql: &str, object: &str, declarations: &str) -> String {
    document(&format!(
        r#"{declarations}<StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan>{}</QueryPlan></StmtSimple>"#,
        scan(1, object)
    ))
}

fn kind(plan: &Plan) -> Kind {
    plan.root.facts().relational_input.unwrap().qualified_kind()
}

#[test]
fn a_closed_apply_uses_the_original_tvp_alias_and_measured_population_without_new_advice() {
    let xml = native(SQL, ROWS, "");
    let plan = parse(SQL, &xml, None);
    assert_eq!(kind(&plan), Kind::TableValuedParameter);
    assert_eq!(plan.root.id, 1);
    assert_eq!(plan.root.facts().table_alias.as_deref(), Some("r"));
    assert_eq!(plan.root.facts().relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(20_000.0));
    assert_eq!(plan.root.act_rows, Some(10_000.0));
    let direct = "SELECT r.id FROM @Rows AS r";
    let direct = parse(direct, &native(direct, ROWS, ""), None);
    let insights = culprit_core::insights::insights(&plan);
    assert_eq!(
        serde_json::to_value(&insights).unwrap(),
        serde_json::to_value(culprit_core::insights::insights(&direct)).unwrap()
    );
    let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(concern.fix_sql.is_none());
    assert!(concern.fix.contains(&culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows")));
    let healthy = xml
        .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
    let healthy = parse(SQL, &healthy, None);
    assert_eq!(kind(&healthy), Kind::TableValuedParameter);
    assert!(
        culprit_core::insights::insights(&healthy)
            .iter()
            .all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access"))
    );
    assert_eq!(
        kind(&crate::plan_xml::parse(&xml).unwrap()),
        Kind::Unknown,
        "the original module cannot be borrowed from SQL shape"
    );
}

#[test]
fn direct_projection_never_promotes_local_scalar_typed_local_or_unknown_inputs_to_tvps() {
    let sql = SQL.replace("@Rows", "@Local");
    let object = ROWS.replace("[@Rows]", "[@Local]");
    for (declaration, expected) in [
        (r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#, Kind::LocalTableVariable),
        (r#"<StmtSimple StatementText="DECLARE @Local int" />"#, Kind::Unknown),
        (r#"<StmtSimple StatementText="DECLARE @Local dbo.RowsType" />"#, Kind::Unknown),
        ("", Kind::Unknown),
    ] {
        let plan = parse(&sql, &native(&sql, &object, declaration), None);
        assert_eq!(kind(&plan), expected, "{declaration}");
        assert_eq!(plan.root.facts().table_alias.as_deref(), Some("r"));
        assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(20_000.0));
        let insights = culprit_core::insights::insights(&plan);
        assert!(insights.iter().all(|insight| insight.fix_sql.is_none()));
    }
}

#[test]
fn apply_cannot_repair_changed_original_statement_alias_object_node_or_expanded_native_scope() {
    let base = native(SQL, ROWS, "");
    for xml in [
        base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[R]\""),
        base.replace("Alias=\"[r]\"", ""),
        base.replace("Table=\"[@Rows]\"", "Table=\"[@Other]\""),
        base.replace("NodeId=\"1\"", ""),
        base.replace("SELECT d.input_id", "SELECT d.other_id"),
        base.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">"),
        base.replace("<QueryPlan>", "<QueryPlan><UDF />"),
        base.replace("<Object Table=", "<Object Server=\"[linked]\" Table="),
        base.replace("</QueryPlan>", &format!("{}</QueryPlan>", scan(1, ROWS))),
        base.replace("</QueryPlan>", &format!("{}</QueryPlan>", scan(2, ROWS))),
    ] {
        assert_ne!(xml, base);
        let plan = parse(SQL, &xml, None);
        assert_eq!(kind(&plan), Kind::Unknown, "{xml}");
        assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(20_000.0));
    }
}

#[test]
fn matching_native_module_and_object_do_not_authorize_an_unclosed_apply_projection() {
    for sql in [
        SQL.replace("SELECT r.id AS input_id", "SELECT ABS(r.id) AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT r.id AS input_id WHERE r.id>0"),
        SQL.replace("SELECT r.id AS input_id", "SELECT r.id AS input_id FROM @Other AS o"),
        SQL.replace("SELECT r.id AS input_id", "SELECT foreign.id AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT COUNT_BIG(*) AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT TOP(1) r.id AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT r.*"),
        SQL.replace("CROSS APPLY", "OUTER APPLY"),
        SQL.replace(") AS d", ") AS r"),
        format!("{SQL} CROSS APPLY (SELECT d.input_id) AS e"),
    ] {
        let plan = parse(&sql, &native(&sql, ROWS, ""), None);
        assert_eq!(kind(&plan), Kind::Unknown, "{sql}");
    }
}

#[test]
fn cached_and_retained_apply_originals_replay_the_same_exact_binding_and_reject_a_foreign_source_or_false_node() {
    use culprit_core::telemetry::*;
    let sql = format!("{SQL} WHERE d.input_id > 9264");
    let raw = native(&sql, ROWS, "");
    for retained in [false, true] {
        let xml = if retained {
            raw.clone()
        } else {
            raw.lines().filter(|line| !line.contains("<RunTimeInformation>")).collect::<Vec<_>>().join("\n")
        };
        let row = crate::cached_definitions::tests::module_row(&sql, &xml);
        let source = crate::plan_xml::test_source(if retained {
            SnapshotDataset::RetainedPlans
        } else {
            SnapshotDataset::CachedDefinitions
        });
        let mut foreign = source.clone();
        foreign.source = "another-original-source".into();
        let (plan, proof, statement, foreign_error, false_error) = if retained {
            let layout = crate::retained_definitions::Layout {
                maximum_bytes: 32 * 1024,
                maximum_statement_bytes: 4096,
                database_setting: Some(true),
            };
            let original = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
            let plan = crate::plan_xml::parse_retained_definition(&original, &source).unwrap();
            let foreign_error = crate::plan_xml::parse_retained_definition(&original, &foreign).unwrap_err();
            let mut changed = original.clone();
            changed.relational_input_proof.as_mut().unwrap().bindings[0].node_id = 999;
            let false_error = crate::plan_xml::parse_retained_definition(&changed, &source).unwrap_err();
            (plan, original.relational_input_proof.unwrap(), original.statement, foreign_error, false_error)
        } else {
            let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
            let original = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
            let plan = crate::plan_xml::parse_cached_definition(&original, &source).unwrap();
            let foreign_error = crate::plan_xml::parse_cached_definition(&original, &foreign).unwrap_err();
            let mut changed = original.clone();
            changed.relational_input_proof.as_mut().unwrap().bindings[0].node_id = 999;
            let false_error = crate::plan_xml::parse_cached_definition(&changed, &source).unwrap_err();
            (plan, original.relational_input_proof.unwrap(), original.statement, foreign_error, false_error)
        };
        assert_eq!(kind(&plan), Kind::TableValuedParameter);
        assert_eq!(plan.root.facts().table_alias.as_deref(), Some("r"));
        assert_eq!(proof.bindings.len(), 1);
        assert_eq!(proof.bindings[0].node_id, 1);
        assert_eq!(
            plan.kind,
            if retained { culprit_core::PlanKind::Actual } else { culprit_core::PlanKind::Estimated }
        );
        for error in [foreign_error, false_error] {
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        }
        let DefinitionPayload::Available { text, .. } = statement.payload else { panic!("original body") };
        assert!(text.contains("CROSS APPLY"));
        assert!(!text.contains("9264"), "retained SQL literals remain redacted independently of native proof hashes");
    }
}
