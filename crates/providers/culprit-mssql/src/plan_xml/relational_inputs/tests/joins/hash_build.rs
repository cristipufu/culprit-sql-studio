use super::*;

fn hash_join(left: &str, right: &str) -> String {
    join(SQL, left, right, "")
        .replace("PhysicalOp=\"Nested Loops\"", "PhysicalOp=\"Hash Match\"")
        .replace("<NestedLoops>", r#"<Hash><HashKeysBuild><ColumnReference Table="[@Rows]" Alias="[r]" Column="id" /></HashKeysBuild><HashKeysProbe><ColumnReference Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" /></HashKeysProbe>"#)
        .replace("</NestedLoops>", "</Hash>")
}

fn native_hash() -> String {
    hash_join(&scan(1, ROWS), &scan(2, ITEMS))
}

fn input_counts() -> String {
    culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K")
}

fn assert_incomplete(plan: &Plan) {
    let observed = culprit_core::insights::insights(plan);
    assert!(observed.iter().all(|insight| !insight.detail.contains(&input_counts())));
    assert!(observed.iter().any(|insight| matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")));
}

#[test]
fn original_native_inner_hash_build_binds_full_tvp_consumption_without_conflating_join_output() {
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(SQL, &native_hash(), Some(&read));
    assert_eq!(plan.root.facts().hash_build_input, Some(1));
    assert_eq!(plan.root.facts().inner_join, Some(true));
    assert_eq!(plan.root.facts().parallel_operator, Some(false));
    assert_eq!(plan.root.facts().serial_execution_count.as_deref(), Some("1"));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert_eq!(plan.root.children[0].facts().access_rows.unwrap().actual_total, Some(20_000.0));
    assert_eq!(plan.root.children[0].act_rows, Some(10_000.0));
    assert_eq!(plan.root.act_rows, Some(100.0));
    let concern = culprit_core::insights::insights(&plan)
        .into_iter()
        .find(|insight| insight.nodes == vec![1] && insight.rule == "misestimate")
        .unwrap();
    assert!(concern.detail.contains(&input_counts()));
    assert!(concern.detail.contains(&culprit_core::t!("insight.relational_input.root_output", actual = "100")));
    assert!(concern.fix.ends_with(&culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows")));
    assert!(concern.fix_sql.is_none());
}

#[test]
fn a_completed_native_tvp_probe_is_still_only_access_evidence() {
    let read = crate::plan_xml::test_read_context("app");
    let native = hash_join(&scan(2, ITEMS), &scan(1, ROWS))
        .replace(r#"<HashKeysBuild><ColumnReference Table="[@Rows]" Alias="[r]" Column="id" /></HashKeysBuild>"#,
            r#"<HashKeysBuild><ColumnReference Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" /></HashKeysBuild>"#)
        .replace(r#"<HashKeysProbe><ColumnReference Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" /></HashKeysProbe>"#,
            r#"<HashKeysProbe><ColumnReference Table="[@Rows]" Alias="[r]" Column="id" /></HashKeysProbe>"#);
    let plan = parse(SQL, &native, Some(&read));
    assert_eq!(plan.root.facts().hash_build_input, Some(2));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(plan.root.children[1].facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    assert_incomplete(&plan);
}

#[test]
fn native_hash_role_refuses_other_algorithms_logical_semantics_adaptation_and_ambiguous_inputs() {
    let read = crate::plan_xml::test_read_context("app");
    let native = native_hash();
    for (case, xml) in [
        ("nested", native.replace("PhysicalOp=\"Hash Match\"", "PhysicalOp=\"Nested Loops\"")),
        ("semi", native.replace("LogicalOp=\"Inner Join\"", "LogicalOp=\"Left Semi Join\"")),
        ("anti", native.replace("LogicalOp=\"Inner Join\"", "LogicalOp=\"Left Anti Semi Join\"")),
        ("outer", native.replace("LogicalOp=\"Inner Join\"", "LogicalOp=\"Left Outer Join\"")),
        ("adaptive-wrapper", native.replace("<Hash>", "<AdaptiveJoin>").replace("</Hash>", "</AdaptiveJoin>")),
        ("adaptive", native.replacen("PhysicalOp=\"Hash Match\"", "PhysicalOp=\"Hash Match\" IsAdaptive=\"true\"", 1)),
        ("unknown-adaptive", native.replacen("PhysicalOp=\"Hash Match\"", "PhysicalOp=\"Hash Match\" IsAdaptive=\"unknown\"", 1)),
        ("adaptive-threshold", native.replacen("PhysicalOp=\"Hash Match\"", "PhysicalOp=\"Hash Match\" AdaptiveThresholdRows=\"100\"", 1)),
        ("parallel", native.replacen("Parallel=\"0\"", "Parallel=\"1\"", 1)),
        ("unknown-parallel", native.replacen("Parallel=\"0\"", "Parallel=\"unknown\"", 1)),
        ("missing-parallel", native.replacen("Parallel=\"0\"", "", 1)),
        ("bitmap", native.replace("<Hash>", "<Hash BitmapCreator=\"true\">")),
        ("star-join", native.replace("<Hash>", "<Hash><StarJoinInfo />")),
        ("missing-probe-keys", native.replace(r#"<HashKeysProbe><ColumnReference Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" /></HashKeysProbe>"#, "")),
        ("empty-build-keys", native.replace(r#"<ColumnReference Table="[@Rows]" Alias="[r]" Column="id" />"#, "")),
        ("duplicate-keys", native.replace("<Hash>", "<Hash><HashKeysBuild><ColumnReference Column=\"id\" /></HashKeysBuild>")),
        ("extra-input", native.replace("</Hash>", &format!("{}</Hash>", scan(3, ITEMS)))),
        ("missing-id", native.replace("NodeId=\"2\"", "")),
        ("duplicate-id", native.replace("NodeId=\"2\"", "NodeId=\"1\"")),
    ] {
        let plan = parse(SQL, &xml, Some(&read));
        assert!(plan.root.facts().hash_build_input.is_none(), "{case}");
        if matches!(case, "duplicate-id" | "missing-id") {
            let observed = culprit_core::insights::insights(&plan);
            assert!(observed.iter().all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")));
            assert!(observed.iter().all(|insight| insight.fix_sql.is_none()));
        } else {
            assert_incomplete(&plan);
        }
    }
}

#[test]
fn supplied_runtime_and_endpoint_contradictions_cannot_launder_native_hash_ownership() {
    let read = crate::plan_xml::test_read_context("app");
    for (case, xml) in [
        ("repeated-hash", native_hash().replacen("ActualExecutions=\"1\"", "ActualExecutions=\"2\"", 1)),
        ("missing-hash-runtime", native_hash().replacen(r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1" /></RunTimeInformation>"#, "", 1)),
        ("parallel-probe", hash_join(&scan(1, ROWS), &scan(2, ITEMS).replace("Parallel=\"0\"", "Parallel=\"1\""))),
        ("repeated-probe", hash_join(&scan(1, ROWS), &scan(2, ITEMS).replace("ActualExecutions=\"1\"", "ActualExecutions=\"2\""))),
        ("unfinished-input", hash_join(&scan(1, ROWS).replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"0\""), &scan(2, ITEMS))),
    ] {
        let plan = parse(SQL, &xml, Some(&read));
        assert_eq!(plan.root.facts().hash_build_input, Some(1), "{case}: role and measured completeness remain separate");
        assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
        assert_incomplete(&plan);
    }
}

#[test]
fn original_module_namespace_and_native_access_identity_remain_required_for_hash_build_tvp_proof() {
    let native = native_hash();
    for read in [None, Some(crate::plan_xml::test_read_context("other"))] {
        let plan = parse(SQL, &native, read.as_ref());
        assert_eq!(kinds(&plan), vec![Kind::Unknown]);
        assert_eq!(plan.root.facts().hash_build_input, Some(1));
        assert_incomplete(&plan);
    }
    let read = crate::plan_xml::test_read_context("app");
    let foreign = native
        .replace("Table=\"[Items]\" Alias=\"[i]\" /></TableScan>", "Table=\"[Foreign]\" Alias=\"[i]\" /></TableScan>");
    let plan = parse(SQL, &foreign, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::Unknown]);
    assert_incomplete(&plan);
}

#[test]
fn original_hash_build_role_replays_from_cached_and_retained_payloads_without_current_schema_authority() {
    use culprit_core::telemetry::*;
    for retained in [false, true] {
        let raw = native_hash();
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
        let row = crate::cached_definitions::tests::module_row(SQL, &xml);
        let plan = if retained {
            let layout = crate::retained_definitions::Layout {
                maximum_bytes: 32 * 1024,
                maximum_statement_bytes: 4096,
                database_setting: Some(true),
            };
            let original = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
            assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
            crate::plan_xml::parse_retained_definition(&original, &source).unwrap()
        } else {
            let limits = crate::cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
            let original = crate::cached_definitions::decode(&row, &limits, Some(&source)).unwrap();
            assert_eq!(original.relational_input_proof.as_ref().unwrap().bindings[0].node_id, 1);
            crate::plan_xml::parse_cached_definition(&original, &source).unwrap()
        };
        assert_eq!(plan.root.facts().hash_build_input, Some(1));
        assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
        let observed = culprit_core::insights::insights(&plan);
        assert_eq!(observed.iter().any(|insight| insight.detail.contains(&input_counts())), retained);
    }
}

#[test]
fn any_supplied_native_goal_on_hash_or_input_withholds_proof_even_when_numeric_facts_become_unknown() {
    let read = crate::plan_xml::test_read_context("app");
    for goal in ["0", "100", "unknown", "-1", "NaN", "inf", "1e309"] {
        for owner in ["hash", "build", "probe"] {
            let attribute = format!(" EstimateRowsWithoutRowGoal=\"{goal}\"");
            let xml = match owner {
                "hash" => native_hash().replacen(
                    "PhysicalOp=\"Hash Match\"",
                    &format!("PhysicalOp=\"Hash Match\"{attribute}"),
                    1,
                ),
                "build" => hash_join(
                    &scan(1, ROWS)
                        .replace("PhysicalOp=\"Table Scan\"", &format!("PhysicalOp=\"Table Scan\"{attribute}")),
                    &scan(2, ITEMS),
                ),
                "probe" => hash_join(
                    &scan(1, ROWS),
                    &scan(2, ITEMS)
                        .replace("PhysicalOp=\"Table Scan\"", &format!("PhysicalOp=\"Table Scan\"{attribute}")),
                ),
                _ => unreachable!(),
            };
            let plan = parse(SQL, &xml, Some(&read));
            assert_eq!(plan.root.facts().hash_build_input, None, "{owner}/{goal}");
            if matches!(goal, "unknown" | "-1" | "NaN" | "inf" | "1e309") {
                let owner_id = match owner {
                    "hash" => 0,
                    "build" => 1,
                    "probe" => 2,
                    _ => unreachable!(),
                };
                assert!(
                    plan.nodes()
                        .into_iter()
                        .find(|node| node.id == owner_id)
                        .unwrap()
                        .facts()
                        .estimated_rows_without_goal
                        .is_none()
                );
            }
            assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
            assert_incomplete(&plan);
        }
    }
}

#[test]
fn malformed_native_ancestor_goals_cannot_be_repaired_by_unknown_typed_numbers_above_hash_build() {
    let read = crate::plan_xml::test_read_context("app");
    for (physical, wrapper) in [("Stream Aggregate", "StreamAggregate"), ("Compute Scalar", "ComputeScalar")] {
        for goal in ["100", "unknown", "-1", "NaN", "inf", "1e309"] {
            let xml = native_hash().replace("<QueryPlan>", &format!(
                r#"<QueryPlan><RelOp NodeId="9" PhysicalOp="{physical}" LogicalOp="Aggregate" Parallel="0" EstimateRows="1" EstimateRowsWithoutRowGoal="{goal}"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation><{wrapper}>"#
            )).replace("</QueryPlan>", &format!("</{wrapper}></RelOp></QueryPlan>"));
            let plan = parse(SQL, &xml, Some(&read));
            let hash = &plan.root.children[0];
            assert_eq!(hash.facts().hash_build_input, None, "{physical}/{goal}");
            if goal != "100" {
                assert!(plan.root.facts().estimated_rows_without_goal.is_none());
            }
            assert_eq!(hash.children[0].facts().relational_input.unwrap().qualified_kind(), Kind::TableValuedParameter);
            assert_incomplete(&plan);
        }
    }
}
