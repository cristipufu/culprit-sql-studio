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

const CHAIN_SQL: &str = "WITH seed AS (SELECT r.id FROM @Rows AS r), input AS (SELECT p.id FROM seed AS p) SELECT d.id FROM input AS d JOIN dbo.Items AS i ON d.id=i.id";

#[test]
fn two_closed_ctes_preserve_original_native_tvp_alias_counts_advice_and_a_healthy_counterexample() {
    let read = crate::plan_xml::test_read_context("app");
    let xml = join(CHAIN_SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let plan = parse(CHAIN_SQL, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    let input = &plan.root.children[0];
    assert_eq!(input.facts().table_alias.as_deref(), Some("r"));
    assert_eq!(input.facts().relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20_000.0));
    assert_eq!(input.act_rows, Some(10_000.0));
    let original = parse(SQL, &join(SQL, &scan(1, ROWS), &scan(2, ITEMS), ""), Some(&read));
    assert_eq!(
        serde_json::to_value(culprit_core::insights::insights(&plan)).unwrap(),
        serde_json::to_value(culprit_core::insights::insights(&original)).unwrap(),
        "a second passthrough does not invent new advice or change existing output"
    );
    let healthy = xml
        .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
    let healthy = parse(CHAIN_SQL, &healthy, Some(&read));
    assert_eq!(kinds(&healthy), vec![Kind::TableValuedParameter]);
    assert!(
        culprit_core::insights::insights(&healthy)
            .iter()
            .all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access"))
    );
}

#[test]
fn chained_cte_syntax_cannot_repair_native_alias_statement_namespace_node_or_expansion_changes() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(CHAIN_SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    assert_eq!(kinds(&parse(CHAIN_SQL, &base, None)), vec![Kind::Unknown]);
    assert_eq!(
        kinds(&parse(CHAIN_SQL, &base, Some(&crate::plan_xml::test_read_context("other")))),
        vec![Kind::Unknown]
    );
    for xml in [
        base.replace("Alias=\"[r]\"", "Alias=\"[p]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[seed]\""),
        base.replace("Alias=\"[r]\"", "Alias=\"[R]\""),
        base.replace("Table=\"[@Rows]\"", "Table=\"[@Other]\""),
        base.replace("Database=\"[app]\"", "Database=\"[foreign]\""),
        base.replace("NodeId=\"2\"", "NodeId=\"1\""),
        base.replace("NodeId=\"2\"", ""),
        base.replace("SELECT p.id", "SELECT p.other_id"),
        base.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">"),
        base.replace("<Object Database=", "<Object Server=\"[linked]\" Database="),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
        join(CHAIN_SQL, &scan(1, ROWS), "", ""),
    ] {
        assert_ne!(xml, base);
        let observed = kinds(&parse(CHAIN_SQL, &xml, Some(&read)));
        assert!(!observed.is_empty());
        assert!(observed.iter().all(|kind| *kind == Kind::Unknown), "{observed:?}");
    }
}

#[test]
fn chained_local_table_variables_scalar_variables_and_unknown_declarations_keep_their_original_kinds() {
    let sql = CHAIN_SQL
        .replace("r.id", "l.id")
        .replace("@Rows AS r", "@Local AS l")
        .replace("dbo.Items AS i", "@Rows AS r")
        .replace("d.id=i.id", "d.id=r.id");
    for (declaration, expected) in [
        (r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#, Kind::LocalTableVariable),
        (r#"<StmtSimple StatementText="DECLARE @Local int" />"#, Kind::Unknown),
        (r#"<StmtSimple StatementText="DECLARE @Local dbo.RowsType" />"#, Kind::Unknown),
        ("", Kind::Unknown),
    ] {
        let xml = join(&sql, &scan(1, r#"Table="[@Local]" Alias="[l]""#), &scan(2, ROWS), declaration);
        let plan = parse(&sql, &xml, None);
        assert_eq!(kinds(&plan), vec![expected, Kind::TableValuedParameter], "{declaration}");
        assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("l"));
    }
}

#[test]
fn original_native_module_metadata_cannot_authorize_repeated_shadowed_or_nonpassthrough_cte_chains() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in [
        CHAIN_SQL.replace("FROM seed AS p", "FROM input AS p"),
        CHAIN_SQL.replace("FROM seed AS p", "FROM @Rows AS p"),
        CHAIN_SQL.replace("p.id", "r.id").replace("seed AS p", "seed AS r"),
        CHAIN_SQL.replace("d.id", "p.id").replace("input AS d", "input AS p"),
        CHAIN_SQL.replace("input AS (", "SEED AS ("),
        CHAIN_SQL.replace("FROM seed AS p)", "FROM seed AS p WHERE p.id>0)"),
        CHAIN_SQL.replace("SELECT p.id", "SELECT ABS(p.id)"),
        CHAIN_SQL.replace("SELECT p.id", "SELECT foreign.id"),
        format!("{CHAIN_SQL} JOIN seed AS s ON d.id=s.id"),
        format!("{CHAIN_SQL} JOIN input AS e ON d.id=e.id"),
        format!("{CHAIN_SQL} CROSS APPLY dbo.expand(d.id) AS x"),
    ] {
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{sql}");
    }
}

#[test]
fn original_cached_and_retained_chain_bindings_replay_without_current_namespace_or_module_inference() {
    use culprit_core::telemetry::*;
    for retained in [false, true] {
        let raw = join(CHAIN_SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
        let xml = if retained {
            raw
        } else {
            raw.lines().filter(|line| !line.contains("<RunTimeInformation>")).collect::<Vec<_>>().join("\n")
        };
        let source = crate::plan_xml::test_source(if retained {
            SnapshotDataset::RetainedPlans
        } else {
            SnapshotDataset::CachedDefinitions
        });
        let row = crate::cached_definitions::tests::module_row(CHAIN_SQL, &xml);
        let (plan, missing) = if retained {
            let layout = crate::retained_definitions::Layout {
                maximum_bytes: 32 * 1024,
                maximum_statement_bytes: 4096,
                database_setting: Some(true),
            };
            let original = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
            assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
            let plan = crate::plan_xml::parse_retained_definition(&original, &source).unwrap();
            let mut wrong = original.clone();
            wrong.module_parameters = None;
            assert!(
                crate::plan_xml::parse_retained_definition(&wrong, &source).is_err(),
                "a retained proof cannot replace the original module declaration"
            );
            let mut other = source.clone();
            other.source = "foreign-original-capture".into();
            assert!(crate::plan_xml::parse_retained_definition(&original, &other).is_err());
            let mut missing = original;
            missing.relational_input_proof = None;
            (plan, crate::plan_xml::parse_retained_definition(&missing, &source).unwrap())
        } else {
            let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
            let original = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
            assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
            let plan = crate::plan_xml::parse_cached_definition(&original, &source).unwrap();
            let mut wrong = original.clone();
            wrong.module_parameters = None;
            assert!(
                crate::plan_xml::parse_cached_definition(&wrong, &source).is_err(),
                "a retained proof cannot replace the original module declaration"
            );
            let mut other = source.clone();
            other.source = "foreign-original-capture".into();
            assert!(crate::plan_xml::parse_cached_definition(&original, &other).is_err());
            let mut missing = original;
            missing.relational_input_proof = None;
            (plan, crate::plan_xml::parse_cached_definition(&missing, &source).unwrap())
        };
        assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
        assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("r"));
        assert_eq!(kinds(&missing), vec![Kind::Unknown]);
    }
}

#[test]
fn a_second_cte_cannot_supply_missing_eof_access_or_serial_ancestor_proof() {
    let sql = "WITH seed AS (SELECT r.id FROM @Rows AS r), input AS (SELECT p.id FROM seed AS p) SELECT COUNT_BIG(*) FROM input AS d WHERE (d.id > 9264)";
    let original = expressions::counted(sql);
    for (xml, complete) in [
        (original.clone(), true),
        (original.replace(" ActualEndOfScans=\"1\"", ""), false),
        (original.replace(" ActualRowsRead=\"20000\"", ""), false),
        (original.replacen("Parallel=\"0\"", "", 1), false),
    ] {
        let plan = parse(sql, &xml, None);
        let input = &plan.root.children[0];
        assert_eq!(input.facts().relational_input.unwrap().qualified_kind(), Kind::TableValuedParameter);
        let insights = culprit_core::insights::insights(&plan);
        let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
        assert_eq!(concern.detail.contains(&expressions::completed_counts()), complete);
        assert!(concern.fix_sql.is_none());
    }
}
