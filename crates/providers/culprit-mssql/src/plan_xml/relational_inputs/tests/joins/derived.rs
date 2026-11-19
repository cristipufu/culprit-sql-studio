use super::*;

pub(super) const SQL: &str = "SELECT d.id FROM (SELECT * FROM @Rows AS r) AS d JOIN dbo.Items AS i ON d.id=i.id";

#[test]
fn a_closed_derived_input_uses_the_original_inner_alias_and_captured_namespace() {
    let read = crate::plan_xml::test_read_context("app");
    let xml = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let plan = parse(SQL, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    let input = &plan.root.children[0];
    assert_eq!(input.facts().table_alias.as_deref(), Some("r"));
    assert_eq!(input.facts().relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20000.0));
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
    assert!(
        insights.iter().all(|insight| !insight.detail.contains(&culprit_core::t!(
            "insight.relational_input.completed_input_counts",
            estimated = "100",
            actual = "20K"
        ))),
        "a derived SQL wrapper does not establish the entire join input population"
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
fn a_passthrough_preserves_local_scalar_and_unknown_input_provenance() {
    let sql = "SELECT d.id FROM (SELECT * FROM @Local AS r) AS d JOIN @Rows AS i ON d.id=i.id";
    let objects = (r#"Table="[@Local]" Alias="[r]""#, r#"Table="[@Rows]" Alias="[i]""#);
    for (declaration, expected) in [
        (r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#, Kind::LocalTableVariable),
        (r#"<StmtSimple StatementText="DECLARE @Local int" />"#, Kind::Unknown),
        (r#"<StmtSimple StatementText="DECLARE @Local dbo.RowsType" />"#, Kind::Unknown),
        ("", Kind::Unknown),
    ] {
        let xml = join(sql, &scan(1, objects.0), &scan(2, objects.1), declaration);
        let plan = parse(sql, &xml, None);
        assert_eq!(kinds(&plan), vec![expected, Kind::TableValuedParameter], "{declaration}");
        assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("r"));
    }
}

#[test]
fn derived_syntax_cannot_repair_changed_native_objects_statement_or_expanded_scope() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    for xml in [
        base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
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
    ] {
        assert_ne!(xml, base);
        let observed = kinds(&parse(SQL, &xml, Some(&read)));
        assert!(!observed.is_empty(), "{xml}");
        assert!(observed.iter().all(|kind| *kind == Kind::Unknown), "{xml}: {observed:?}");
    }
    let extra = base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS)));
    assert_eq!(kinds(&parse(SQL, &extra, Some(&read))), vec![Kind::Unknown, Kind::Unknown]);
}

#[test]
fn native_object_and_original_module_matches_do_not_authorize_unclosed_derived_sql() {
    let read = crate::plan_xml::test_read_context("app");
    for from in [
        "(SELECT id FROM @Rows AS r) AS d",
        "(SELECT * FROM @Rows AS r WHERE r.id > 0) AS d",
        "(SELECT TOP(1) * FROM @Rows AS r) AS d",
        "(SELECT COUNT_BIG(*) FROM @Rows AS r) AS d",
        "(SELECT * FROM @Rows AS r) AS R",
        "(SELECT * FROM (SELECT * FROM @Rows AS r) AS nested) AS d",
        "(SELECT dbo.hidden(r.id) FROM @Rows AS r) AS d",
    ] {
        let sql = format!("SELECT d.id FROM {from} JOIN dbo.Items AS i ON d.id=i.id");
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{sql}");
    }
}
