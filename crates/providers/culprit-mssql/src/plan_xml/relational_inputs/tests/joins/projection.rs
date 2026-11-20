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
fn foreign_ambiguous_and_computed_columns_remain_unknown_with_original_module_proof() {
    let read = crate::plan_xml::test_read_context("app");
    for projection in [
        "id",
        "other.id",
        "r.id,other.Code",
        "r.id,r.id",
        "r.Id,r.id",
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

fn aliased_statements() -> [String; 2] {
    [DERIVED_SQL, CTE_SQL]
        .map(|sql| sql.replace("r.id,r.Code", "r.id AS input_id,r.Code AS input_code").replace("d.id", "d.input_id"))
}

#[test]
fn explicit_output_column_aliases_keep_original_native_tvp_ownership_counts_and_healthy_controls() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in aliased_statements() {
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        let plan = parse(&sql, &xml, Some(&read));
        let input = &plan.root.children[0];
        assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
        assert_eq!(input.facts().table_alias.as_deref(), Some("r"));
        assert_eq!(input.facts().relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
        assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20_000.0));
        assert_eq!(input.act_rows, Some(10_000.0));
        let observed = culprit_core::insights::insights(&plan);
        let concern =
            observed.iter().find(|insight| insight.rule == "misestimate" && insight.nodes == vec![1]).unwrap();
        assert!(concern.fix.ends_with(&culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows")));
        assert!(concern.fix_sql.is_none());
        assert!(!concern.detail.contains(&expressions::completed_counts()));
        let healthy = xml
            .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
            .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
        let healthy = parse(&sql, &healthy, Some(&read));
        assert_eq!(kinds(&healthy), vec![Kind::TableValuedParameter]);
        assert!(
            culprit_core::insights::insights(&healthy)
                .iter()
                .all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access"))
        );
    }
}

#[test]
fn aliases_do_not_repair_changed_original_statement_native_input_or_database_scope() {
    let read = crate::plan_xml::test_read_context("app");
    for sql in aliased_statements() {
        let base = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &base, None)), vec![Kind::Unknown]);
        for xml in [
            base.replace("Alias=\"[r]\"", "Alias=\"[d]\""),
            base.replace("Alias=\"[r]\"", "Alias=\"[R]\""),
            base.replace("Table=\"[@Rows]\"", "Table=\"[@Other]\""),
            base.replace("Database=\"[app]\"", "Database=\"[other]\""),
            base.replace("input_code", "changed_code"),
            base.replace("NodeId=\"2\"", "NodeId=\"1\""),
            base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
        ] {
            let kinds = kinds(&parse(&sql, &xml, Some(&read)));
            assert!(!kinds.is_empty());
            assert!(kinds.iter().all(|kind| *kind == Kind::Unknown), "{sql}: {kinds:?}");
        }
    }
}

#[test]
fn explicit_output_aliases_do_not_promote_local_scalar_or_unresolved_inputs_to_tvp() {
    for sql in aliased_statements() {
        let sql = sql
            .replace("r.", "l.")
            .replace("@Rows AS r", "@Local AS l")
            .replace("dbo.Items AS i", "@Rows AS r")
            .replace("d.input_id=i.id", "d.input_id=r.id");
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
            assert_eq!(kinds(&plan), vec![expected, Kind::TableValuedParameter], "{sql}: {declaration}");
        }
    }
}

#[test]
fn original_cached_and_retained_alias_bindings_replay_without_reconstructing_current_names() {
    use culprit_core::telemetry::*;
    for sql in aliased_statements() {
        for retained in [false, true] {
            let raw = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
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
            let row = crate::cached_definitions::tests::module_row(&sql, &xml);
            let (plan, missing) = if retained {
                let layout = crate::retained_definitions::Layout {
                    maximum_bytes: 32 * 1024,
                    maximum_statement_bytes: 4096,
                    database_setting: Some(true),
                };
                let original = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
                assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
                let plan = crate::plan_xml::parse_retained_definition(&original, &source).unwrap();
                let mut missing = original;
                missing.relational_input_proof = None;
                (plan, crate::plan_xml::parse_retained_definition(&missing, &source).unwrap())
            } else {
                let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
                let original = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
                assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
                let plan = crate::plan_xml::parse_cached_definition(&original, &source).unwrap();
                let mut missing = original;
                missing.relational_input_proof = None;
                (plan, crate::plan_xml::parse_cached_definition(&missing, &source).unwrap())
            };
            assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
            assert_eq!(plan.root.children[0].facts().table_alias.as_deref(), Some("r"));
            assert_eq!(kinds(&missing), vec![Kind::Unknown]);
        }
    }
}

#[test]
fn explicit_column_names_do_not_supply_missing_scan_completion_or_ancestor_runtime_proof() {
    for sql in [
        "SELECT COUNT_BIG(*) FROM (SELECT r.id AS input_id FROM @Rows AS r) AS d WHERE (d.input_id > 9264)",
        "WITH input AS (SELECT r.id AS input_id FROM @Rows AS r) SELECT COUNT_BIG(*) FROM input AS d WHERE (d.input_id > 9264)",
    ] {
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
            let observed = culprit_core::insights::insights(&plan);
            let concern = observed.iter().find(|insight| insight.rule == "misestimate").unwrap();
            assert_eq!(concern.detail.contains(&expressions::completed_counts()), complete);
            assert!(concern.fix_sql.is_none());
        }
    }
}
