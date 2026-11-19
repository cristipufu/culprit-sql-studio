use super::*;

pub(super) const SQL: &str =
    "WITH input AS (SELECT * FROM @Rows AS r) SELECT d.id FROM input AS d JOIN dbo.Items AS i ON d.id=i.id";

#[test]
fn one_closed_cte_keeps_original_native_alias_module_namespace_and_measured_rows() {
    let read = crate::plan_xml::test_read_context("app");
    let xml = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let plan = parse(SQL, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    let input = &plan.root.children[0];
    assert_eq!(input.facts().table_alias.as_deref(), Some("r"));
    assert_eq!(input.facts().relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20000.0));
    assert_eq!(input.act_rows, Some(10000.0));
    let insights = culprit_core::insights::insights(&plan);
    let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert_eq!(
        concern.fix,
        format!(
            "{} {}",
            culprit_core::t!("insight.relational_input.investigate_volume"),
            culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows")
        )
    );
    assert!(concern.fix_sql.is_none());
    let completed =
        culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K");
    assert!(
        insights.iter().all(|insight| !insight.detail.contains(&completed)),
        "a CTE cannot establish a completed population under a join"
    );
    assert_eq!(kinds(&parse(SQL, &xml, None)), vec![Kind::Unknown]);
    assert_eq!(kinds(&parse(SQL, &xml, Some(&crate::plan_xml::test_read_context("other")))), vec![Kind::Unknown]);
    assert_eq!(kinds(&crate::plan_xml::parse(&xml).unwrap()), vec![Kind::Unknown]);
    let healthy = xml
        .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
    let plan = parse(SQL, &healthy, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
}

#[test]
fn closed_ctes_preserve_local_table_variable_scalar_and_unknown_declaration_kinds() {
    let sql = "WITH input AS (SELECT * FROM @Local AS l) SELECT d.id FROM input AS d JOIN @Rows AS r ON d.id=r.id";
    for (declaration, expected) in [
        (r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#, Kind::LocalTableVariable),
        (r#"<StmtSimple StatementText="DECLARE @Local int" />"#, Kind::Unknown),
        (r#"<StmtSimple StatementText="DECLARE @Local dbo.RowsType" />"#, Kind::Unknown),
        ("", Kind::Unknown),
    ] {
        let xml = join(sql, &scan(1, r#"Table="[@Local]" Alias="[l]""#), &scan(2, ROWS), declaration);
        let plan = parse(sql, &xml, None);
        assert_eq!(kinds(&plan), vec![expected, Kind::TableValuedParameter], "{declaration}");
        assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("l"));
    }
}

#[test]
fn a_cte_never_repairs_changed_original_statement_objects_aliases_node_ids_or_native_scope() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    for xml in [
        base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[input]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[R]\""),
        base.replace("Alias=\"[r]\"", ""),
        base.replace("Database=\"[app]\"", "Database=\"[other]\""),
        base.replace("NodeId=\"2\"", "NodeId=\"1\""),
        base.replace("NodeId=\"2\"", ""),
        join(SQL, &scan(1, ROWS), "", ""),
        base.replace("<Object Database=", "<Object Server=\"[linked]\" Database="),
        base.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">"),
        base.replace("<QueryPlan>", "<QueryPlan><UDF />"),
        base.replace("SELECT d.id", "SELECT d.other_id"),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
    ] {
        assert_ne!(xml, base);
        let observed = kinds(&parse(SQL, &xml, Some(&read)));
        assert!(!observed.is_empty(), "{xml}");
        assert!(observed.iter().all(|kind| *kind == Kind::Unknown), "{xml}: {observed:?}");
    }
}

#[test]
fn original_module_proof_and_matching_objects_do_not_authorize_other_cte_forms() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in [
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT id FROM @Rows AS r"),
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT * FROM @Rows AS r WHERE r.id > 0"),
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT TOP(1) * FROM @Rows AS r"),
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT COUNT_BIG(*) FROM @Rows AS r"),
        SQL.replace("input AS (", "input(id) AS ("),
        SQL.replace("input AS d", "input d"),
        SQL.replace("input AS d", "input AS r"),
        SQL.replace("SELECT d.id", "SELECT dbo.hidden(d.id)"),
        format!("{SQL} JOIN input AS e ON d.id=e.id"),
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT * FROM input AS r"),
        SQL.replace("SELECT * FROM @Rows AS r", "SELECT * FROM dbo.expand(@Rows) AS r"),
    ] {
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{sql}");
    }
}
