use super::*;

pub(super) const DERIVED_SQL: &str =
    "SELECT d.id FROM (SELECT r.id,r.Code FROM @Rows AS r) AS d JOIN dbo.Items AS i ON d.id=i.id";
pub(super) const CTE_SQL: &str =
    "WITH input AS (SELECT r.id,r.Code FROM @Rows AS r) SELECT d.id FROM input AS d JOIN dbo.Items AS i ON d.id=i.id";

fn statements(projection: &str) -> [String; 2] {
    [DERIVED_SQL.replace("r.id,r.Code", projection), CTE_SQL.replace("r.id,r.Code", projection)]
}

#[test]
fn direct_projected_columns_keep_original_tvp_provenance_native_counts_and_healthy_counterexamples() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in [DERIVED_SQL, CTE_SQL] {
        let xml = join(sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        let plan = parse(sql, &xml, Some(&read));
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
        assert!(insights.iter().all(|insight| !insight.detail.contains(&expressions::completed_counts())));
        assert_eq!(kinds(&parse(sql, &xml, None)), vec![Kind::Unknown]);
        let healthy = xml
            .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
            .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
        let healthy = parse(sql, &healthy, Some(&read));
        assert_eq!(kinds(&healthy), vec![Kind::TableValuedParameter]);
        assert!(!culprit_core::insights::insights(&healthy).iter().any(|insight| insight.rule == "misestimate"));
    }
}

#[test]
fn projected_sql_cannot_repair_a_changed_original_native_alias_namespace_statement_or_inventory() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in [DERIVED_SQL, CTE_SQL] {
        let base = join(sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        for xml in [
            base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
            base.replace("Alias=\"[r]\"", "Alias=\"[R]\""),
            base.replace("Alias=\"[r]\"", ""),
            base.replace("Table=\"[@Rows]\"", "Table=\"[@Other]\""),
            base.replace("Database=\"[app]\"", "Database=\"[other]\""),
            base.replace("NodeId=\"2\"", "NodeId=\"1\""),
            base.replace("NodeId=\"2\"", ""),
            base.replace("r.id,r.Code", "r.id,r.OtherCode"),
            base.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">"),
            base.replace("<Object Database=", "<Object Server=\"[linked]\" Database="),
            base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
            join(sql, &scan(1, ROWS), "", ""),
        ] {
            assert_ne!(xml, base);
            let observed = kinds(&parse(sql, &xml, Some(&read)));
            assert!(!observed.is_empty());
            assert!(observed.iter().all(|kind| *kind == Kind::Unknown), "{sql}: {observed:?}");
        }
    }
}

#[test]
fn projected_local_scalar_and_unknown_inputs_do_not_borrow_tvp_parameter_metadata() {
    for sql in [DERIVED_SQL, CTE_SQL] {
        let sql = sql
            .replace("r.id,r.Code", "l.id,l.Code")
            .replace("@Rows AS r", "@Local AS l")
            .replace("dbo.Items AS i", "@Rows AS r")
            .replace("d.id=i.id", "d.id=r.id");
        for (declaration, expected) in [
            (
                r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int,Code varchar(16))" />"#,
                Kind::LocalTableVariable,
            ),
            (r#"<StmtSimple StatementText="DECLARE @Local int" />"#, Kind::Unknown),
            (r#"<StmtSimple StatementText="DECLARE @Local dbo.RowsType" />"#, Kind::Unknown),
            ("", Kind::Unknown),
        ] {
            let xml = join(&sql, &scan(1, r#"Table="[@Local]" Alias="[l]""#), &scan(2, ROWS), declaration);
            let plan = parse(&sql, &xml, None);
            assert_eq!(kinds(&plan), vec![expected, Kind::TableValuedParameter], "{sql} {declaration}");
            assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("l"));
        }
    }
}

#[test]
fn direct_projections_do_not_supply_missing_completed_scan_or_serial_ancestor_proof() {
    for sql in [
        "SELECT COUNT_BIG(*) FROM (SELECT r.id,r.Code FROM @Rows AS r) AS d WHERE (d.id > 9264)",
        "WITH input AS (SELECT r.id,r.Code FROM @Rows AS r) SELECT COUNT_BIG(*) FROM input AS d WHERE (d.id > 9264)",
    ] {
        let original = expressions::counted(sql);
        for (xml, completed) in [
            (original.clone(), true),
            (original.replace(" ActualEndOfScans=\"1\"", ""), false),
            (original.replace(" ActualRowsRead=\"20000\"", ""), false),
            (original.replacen("Parallel=\"0\"", "", 1), false),
        ] {
            let plan = parse(sql, &xml, None);
            assert_eq!(
                plan.root.children[0].facts().relational_input.unwrap().qualified_kind(),
                Kind::TableValuedParameter
            );
            let insights = culprit_core::insights::insights(&plan);
            let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
            assert_eq!(concern.detail.contains(&expressions::completed_counts()), completed, "{sql}: {xml}");
            assert!(concern.fix_sql.is_none());
        }
    }
}

#[test]
fn renamed_foreign_ambiguous_and_computed_columns_remain_unknown_with_original_module_proof() {
    let read = crate::plan_xml::test_read_context("app");
    for projection in [
        "id",
        "other.id",
        "r.id,other.Code",
        "r.id,r.id",
        "r.Id,r.id",
        "r.id AS id",
        "r.id other",
        "r.id+1",
        "ABS(r.id)",
        "dbo.hidden(r.id)",
        "r.Code COLLATE Latin1_General_100_CI_AS",
        "DISTINCT r.id",
        "TOP(1) r.id",
        "r.*",
        "r.id,*",
        "COUNT_BIG(*)",
        "r.id,",
        "r.id,,r.Code",
        "(SELECT id FROM @Other)",
    ] {
        for sql in statements(projection) {
            let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
            assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{sql}");
        }
    }
}
