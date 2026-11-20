use super::*;

fn native(runtime: &str, cached: bool) -> Plan {
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
<StmtSimple QueryHash="0xA" StatementText="SELECT value FROM dbo.rows WHERE id=@p" RetrievedFromCache="{cached}">
<QueryPlan><ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(1)" ParameterRuntimeValue="{runtime}"/></ParameterList>
<RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="false" EstimateRows="1000">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualExecutions="1" ActualLogicalReads="2000"/></RunTimeInformation>
<TableScan><Object Schema="[dbo]" Table="[rows]"/></TableScan></RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    crate::plan_xml::parse(&xml).unwrap()
}

#[test]
fn original_native_runtime_integers_are_compared_without_retaining_their_representations() {
    let a = native("(+001)", true);
    let b = native("(1)", false);
    assert_eq!(compare(&a, &b).relation(), Relation::Same);
    let different = native("(2)", false);
    assert_eq!(compare(&a, &different).relation(), Relation::Different);
    assert_eq!(compare(&different, &a).relation(), Relation::Different);
}

#[test]
fn edited_json_facts_or_missing_original_payload_cannot_lend_native_input_or_counter_proof() {
    let a = native("(1)", true);
    let original = native("(1)", false);
    for change in ["raw", "rows", "reads", "cache", "statement", "engine"] {
        let mut b = original.clone();
        match change {
            "raw" => b.raw = None,
            "rows" => b.root.act_rows = Some(999.0),
            "reads" => b.root.runtime.as_mut().unwrap().logical_reads = Some(1.0),
            "cache" => b.facts_mut().parameter_context.as_mut().unwrap().retrieved_from_cache = Some(true),
            "statement" => b.statement = Some("SELECT value FROM foreign.rows WHERE id=@p".into()),
            "engine" => b.engine = Engine::Postgres,
            _ => unreachable!(),
        }
        assert_eq!(compare(&a, &b).relation(), Relation::Unavailable, "{change}");
    }
}

fn measured_pair() -> (Plan, Plan) {
    (
        crate::plan_xml::parse(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/scalar-reuse/lookup.sqlplan"
        )))
        .unwrap(),
        crate::plan_xml::parse(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/scalar-reuse/scan.sqlplan"
        )))
        .unwrap(),
    )
}
fn changed(plan: &Plan, before: &str, after: &str) -> Plan {
    assert!(plan.raw.as_ref().unwrap().contains(before), "the control must change native evidence");
    crate::plan_xml::parse(&plan.raw.as_ref().unwrap().replacen(before, after, 1)).unwrap()
}
fn diff(a: &Plan, name_a: &str, b: &Plan, name_b: &str) -> culprit_core::PlanDiff {
    culprit_core::plan_compare::compare_native_pair(compare(a, b), name_a, name_b, &Default::default())
}
fn observed(diff: &culprit_core::PlanDiff) -> Option<&culprit_core::Scenario> {
    diff.scenarios.iter().find(|scenario| {
        scenario
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&culprit_core::t!("core.plan_compare.scalar_reuse.limit")))
    })
}

#[test]
fn same_native_runtime_tuple_links_material_cached_lookup_and_counterpart_scan_work_in_both_orders() {
    let (a, b) = measured_pair();
    assert_eq!(compare(&a, &b).relation(), Relation::Same);
    let result = diff(&a, "Cached", &b, "Scan");
    let scenario = observed(&result).expect("same original input and measured native lookup-to-scan work");
    assert_eq!((scenario.a, scenario.b), (Some(3), Some(5)));
    let lookup = a.nodes().into_iter().find(|node| node.id == 3).unwrap();
    let scan = b.nodes().into_iter().find(|node| node.id == 5).unwrap();
    assert_eq!(lookup.act_executions, Some(10_000.0));
    assert_eq!(lookup.runtime.as_ref().unwrap().logical_reads, Some(30_000.0));
    assert_eq!(scan.runtime.as_ref().unwrap().logical_reads, Some(2_000.0));
    assert!(scenario.detail.as_ref().unwrap().contains(&culprit_core::t!(
        "core.plan_compare.scalar_reuse.detail",
        reused = "Cached",
        scan = "Scan",
        executions = culprit_core::format::count(10_000.0),
        lookup_reads = culprit_core::format::count(30_000.0),
        scan_reads = culprit_core::format::count(2_000.0)
    )));
    assert_eq!(
        scenario.detail.as_deref(),
        Some(
            format!(
                "{} {}",
                culprit_core::t!(
                    "core.plan_compare.scalar_reuse.detail",
                    reused = "Cached",
                    scan = "Scan",
                    executions = culprit_core::format::count(10_000.0),
                    lookup_reads = culprit_core::format::count(30_000.0),
                    scan_reads = culprit_core::format::count(2_000.0)
                ),
                culprit_core::t!("core.plan_compare.scalar_reuse.limit")
            )
            .as_str()
        )
    );
    let reversed = diff(&b, "Scan", &a, "Cached");
    let reversed = observed(&reversed).unwrap();
    assert_eq!((reversed.a, reversed.b), (Some(5), Some(3)));
    assert_eq!(reversed.title, scenario.title);
    for private in ["314159", "271828", "ParameterRuntimeValue", "ParameterCompiledValue", "ShowPlanXML"] {
        assert!(!serde_json::to_string(&result).unwrap().contains(private), "{private}");
    }
}

#[test]
fn equal_compiled_runtime_integers_keep_material_native_work_comparison_without_parameter_change_advice() {
    let (original, scan) = measured_pair();
    for compiled in ["(314159)", "(+0314159)"] {
        let lookup = changed(
            &original,
            "ParameterCompiledValue=\"(271828)\"",
            &format!("ParameterCompiledValue=\"{compiled}\""),
        );
        assert_eq!(lookup.facts().parameter_context.as_ref().unwrap().parameters[0].integer_values_equal, Some(true));
        assert_eq!(compare(&lookup, &scan).relation(), Relation::Same);
        for reversed in [false, true] {
            let result =
                if reversed { diff(&scan, "Scan", &lookup, "Cached") } else { diff(&lookup, "Cached", &scan, "Scan") };
            let scenario = observed(&result).expect("native equal inputs retain observed work without a cause verdict");
            assert_eq!((scenario.a, scenario.b), if reversed { (Some(5), Some(3)) } else { (Some(3), Some(5)) });
            assert_eq!(
                scenario.detail.as_deref(),
                Some(
                    format!(
                        "{} {} {} {}",
                        culprit_core::t!(
                            "core.plan_compare.scalar_reuse.detail",
                            reused = "Cached",
                            scan = "Scan",
                            executions = culprit_core::format::count(10_000.0),
                            lookup_reads = culprit_core::format::count(30_000.0),
                            scan_reads = culprit_core::format::count(2_000.0)
                        ),
                        culprit_core::t!("core.plan_compare.scalar_reuse.equal_compile_input"),
                        culprit_core::t!("core.plan_compare.scalar_reuse.limit"),
                        culprit_core::t!("core.plan_compare.scalar_reuse.equal_investigation")
                    )
                    .as_str()
                )
            );
            for private in
                ["314159", "271828", "0314159", "ParameterRuntimeValue", "ParameterCompiledValue", "ShowPlanXML"]
            {
                assert!(!serde_json::to_string(&result).unwrap().contains(private), "{private}");
            }
        }
    }
}

#[test]
fn equal_compiled_inputs_do_not_override_unknown_healthy_variant_or_changed_runtime_evidence() {
    let (original, scan) = measured_pair();
    let equal = changed(&original, "ParameterCompiledValue=\"(271828)\"", "ParameterCompiledValue=\"(314159)\"");
    for (before, after) in [
        (" ParameterCompiledValue=\"(314159)\"", ""),
        ("ParameterCompiledValue=\"(314159)\"", "ParameterCompiledValue=\"(?)\""),
        ("EstimateRows=\"100\" EstimatedRowsRead=\"100\"", "EstimateRows=\"10000\" EstimatedRowsRead=\"10000\""),
        ("RetrievedFromCache=\"true\"", "RetrievedFromCache=\"unknown\""),
        (
            "ActualRows=\"10000\" ActualRowsRead=\"10000\" ActualExecutions=\"10000\"",
            "ActualRows=\"9999\" ActualRowsRead=\"10000\" ActualExecutions=\"10000\"",
        ),
        ("ActualLogicalReads=\"30000\"", "ActualLogicalReads=\"NaN\""),
        ("Parallel=\"false\"", "Parallel=\"true\""),
        ("<StmtSimple ", "<StmtSimple QueryVariantID=\"2\" "),
        ("<QueryPlan>", "<QueryPlan><Dispatcher><OptionalParameterPredicate/></Dispatcher>"),
    ] {
        let altered = changed(&equal, before, after);
        assert!(observed(&diff(&altered, "Lookup", &scan, "Scan")).is_none(), "{before} -> {after}");
    }
    let changed_input = changed(&scan, "ParameterRuntimeValue=\"(314159)\"", "ParameterRuntimeValue=\"(314160)\"");
    assert_eq!(compare(&equal, &changed_input).relation(), Relation::Different);
    assert!(observed(&diff(&equal, "Lookup", &changed_input, "Scan")).is_none());
    let unknown = changed(&equal, " ParameterCompiledValue=\"(314159)\"", "");
    assert_eq!(
        compare(&unknown, &scan).relation(),
        Relation::Same,
        "known runtime equality cannot replace unknown compile context"
    );
    assert_eq!(unknown.facts().parameter_context.as_ref().unwrap().parameters[0].integer_values_equal, None);
    let extra = r#"<ColumnReference Column="@other" ParameterDataType="int" ParameterCompiledValue="(2)" ParameterRuntimeValue="(2)"/>"#;
    let complete_lookup = changed(&equal, "</ParameterList>", &format!("{extra}</ParameterList>"));
    let complete_scan = changed(&scan, "</ParameterList>", &format!("{extra}</ParameterList>"));
    assert_eq!(compare(&complete_lookup, &complete_scan).relation(), Relation::Same);
    assert!(observed(&diff(&complete_lookup, "Lookup", &complete_scan, "Scan")).is_some());
    let different_member = changed(&complete_scan, "ParameterRuntimeValue=\"(2)\"", "ParameterRuntimeValue=\"(3)\"");
    assert_eq!(compare(&complete_lookup, &different_member).relation(), Relation::Different);
    assert!(observed(&diff(&complete_lookup, "Lookup", &different_member, "Scan")).is_none());
    assert_eq!(compare(&complete_lookup, &scan).relation(), Relation::Unavailable);
    assert!(observed(&diff(&complete_lookup, "Lookup", &scan, "Scan")).is_none());
}

#[test]
fn counterpart_cache_state_does_not_establish_or_refuse_an_independent_observed_scan_comparison() {
    let (a, original) = measured_pair();
    for cache in ["RetrievedFromCache=\"true\"", "UnknownCacheMetadata=\"false\"", "RetrievedFromCache=\"unknown\""] {
        let counterpart = changed(&original, "RetrievedFromCache=\"false\"", cache);
        assert_eq!(compare(&a, &counterpart).relation(), Relation::Same, "{cache}");
        let result = diff(&a, "Cached", &counterpart, "Scan");
        let scenario = observed(&result).expect("cache state does not change the original scan counters");
        assert_eq!((scenario.a, scenario.b), (Some(3), Some(5)));
        assert_eq!(observed(&diff(&counterpart, "Scan", &a, "Cached")).unwrap().title, scenario.title);
        assert!(scenario.detail.as_ref().unwrap().contains(&culprit_core::t!("core.plan_compare.scalar_reuse.limit")));
    }
}

#[test]
fn native_variants_and_dispatchers_are_unavailable_independently_of_plain_supported_statement_text() {
    let a = native("(1)", true);
    let b = native("(1)", false);
    for variant in ["2", "3", "malformed"] {
        let tagged = changed(&b, "<StmtSimple ", &format!("<StmtSimple QueryVariantID=\"{variant}\" "));
        assert_eq!(tagged.statement, b.statement);
        assert_eq!(compare(&a, &tagged).relation(), Relation::Unavailable);
    }
    for metadata in [" QueryVariantID=\"2\"", " QueryVariantID=\"3\""] {
        let tagged = changed(&b, "<QueryPlan>", &format!("<QueryPlan{metadata}>"));
        assert_eq!(compare(&a, &tagged).relation(), Relation::Unavailable);
    }
    for metadata in [
        "<Dispatcher><ParameterSensitivePredicate/></Dispatcher>",
        "<Dispatcher><OptionalParameterPredicate/></Dispatcher>",
    ] {
        let tagged = changed(&b, "<QueryPlan>", &format!("<QueryPlan>{metadata}"));
        assert_eq!(tagged.statement, b.statement);
        assert_eq!(compare(&a, &tagged).relation(), Relation::Unavailable);
    }
}

#[test]
fn complete_named_tuples_do_not_compare_by_position_or_by_one_matching_member() {
    let a = native("(1)", true);
    let extra = r#"<ColumnReference Column="@other" ParameterDataType="bigint" ParameterCompiledValue="(2)" ParameterRuntimeValue="(2)"/>"#;
    let a = changed(&a, "</ParameterList>", &format!("{extra}</ParameterList>"));
    let b = changed(&a, "<ParameterList>", &format!("<ParameterList>{extra}"));
    assert_eq!(compare(&a, &b).relation(), Relation::Unavailable, "a duplicate is not a complete second tuple");
    let raw = a.raw.as_ref().unwrap();
    let first_start = raw.find("<ColumnReference ").unwrap();
    let first_end = first_start + raw[first_start..].find("/>").unwrap() + 2;
    let first = &raw[first_start..first_end];
    let reordered = raw.replace(&format!("{first}{extra}"), &format!("{extra}{first}"));
    assert_ne!(reordered.as_str(), raw.as_ref());
    let reordered = crate::plan_xml::parse(&reordered).unwrap();
    assert_eq!(compare(&a, &reordered).relation(), Relation::Same);
    let other = changed(&reordered, "ParameterRuntimeValue=\"(2)\"", "ParameterRuntimeValue=\"(3)\"");
    assert_eq!(compare(&a, &other).relation(), Relation::Different);
}

#[test]
fn missing_hidden_unsupported_or_ambiguous_parameter_populations_never_compare_equal() {
    let a = native("(1)", true);
    let b = native("(1)", false);
    for (before, after) in [
        (" ParameterRuntimeValue=\"(1)\"", ""),
        ("ParameterRuntimeValue=\"(1)\"", "ParameterRuntimeValue=\"(?)\""),
        ("ParameterRuntimeValue=\"(1)\"", "ParameterRuntimeValue=\"(NULL)\""),
        ("ParameterRuntimeValue=\"(1)\"", "ParameterRuntimeValue=\"(1+0)\""),
        ("ParameterDataType=\"int\"", "ParameterDataType=\"bigint\""),
        ("ParameterDataType=\"int\"", "ParameterDataType=\"dbo.IntegerAlias\""),
        ("ParameterDataType=\"int\"", "ParameterDataType=\"dbo.RowsType\""),
        ("ParameterRuntimeValue=\"(1)\"", "ParameterRuntimeValue=\"(2147483648)\""),
        ("</ParameterList>", "<UnexpectedInput/></ParameterList>"),
        ("Column=\"@p\"", "Column=\"@other\""),
        ("<ParameterList>", "<ParameterList/><ParameterList>"),
    ] {
        let altered = changed(&b, before, after);
        assert_eq!(compare(&a, &altered).relation(), Relation::Unavailable, "{before} -> {after}");
    }
    let redacted = culprit_core::redact::redact_plan(&b);
    assert_eq!(compare(&a, &redacted).relation(), Relation::Unavailable);
}

#[test]
fn missing_or_hidden_native_branches_and_other_original_statements_do_not_lend_input_scope() {
    let a = native("(1)", true);
    let b = native("(1)", false);
    for (before, after) in [
        ("dbo.rows", "dbo.other_rows"),
        ("FROM dbo.rows", "FROM @rows"),
        ("FROM dbo.rows", "FROM #rows"),
        ("QueryHash=\"0xA\"", "QueryHash=\"0xB\""),
        ("ActualRows=\"1000\"", "ActualRows=\"NaN\""),
        ("ActualExecutions=\"1\"", "ActualExecutions=\"0\""),
        ("ActualLogicalReads=\"2000\"", "ActualLogicalReads=\"2000.00000000000000000001\""),
        (
            "</QueryPlan>",
            "<RelOp NodeId=\"99\" PhysicalOp=\"Table Scan\" LogicalOp=\"Table Scan\" EstimateRows=\"1\"><TableScan><Object Table=\"other\"/></TableScan></RelOp></QueryPlan>",
        ),
    ] {
        let altered = changed(&b, before, after);
        assert_eq!(compare(&a, &altered).relation(), Relation::Unavailable, "{before} -> {after}");
    }
    let original = b.raw.as_ref().unwrap();
    let statement_start = original.find("<StmtSimple ").unwrap();
    let statement_end = original.find("</StmtSimple>").unwrap() + "</StmtSimple>".len();
    let two = original.replace("</Statements>", &format!("{}</Statements>", &original[statement_start..statement_end]));
    let two = crate::plan_xml::parse(&two).unwrap();
    assert_eq!(compare(&a, &two).relation(), Relation::Unavailable);
}

#[test]
fn fractional_native_access_counts_cannot_round_into_a_same_population_comparison() {
    let (a, b) = measured_pair();
    assert!(observed(&diff(&a, "Cached", &b, "Scan")).is_some());
    let rounded = changed(
        &a,
        "ActualRowsRead=\"10000\" ActualExecutions=\"1\" ActualLogicalReads=\"1000\"",
        "ActualRowsRead=\"10000.00000000000000000001\" ActualExecutions=\"1\" ActualLogicalReads=\"1000\"",
    );
    assert_eq!(compare(&rounded, &b).relation(), Relation::Unavailable);
    assert!(observed(&diff(&rounded, "Cached", &b, "Scan")).is_none());
}

#[test]
fn native_equal_inputs_do_not_promote_healthy_incomplete_or_competing_work_populations() {
    let (a, b) = measured_pair();
    for (before, after) in [
        ("ActualLogicalReads=\"2000\"", "ActualLogicalReads=\"29000\""),
        (" ActualLogicalReads=\"2000\"", ""),
        ("ActualRows=\"10000\"", "ActualRows=\"9999\""),
        ("NodeId=\"5\"", "NodeId=\"0\""),
        ("PhysicalOp=\"Clustered Index Scan\"", "PhysicalOp=\"Index Seek\""),
        ("Table=\"[rows]\"", "Table=\"[other]\""),
        ("Alias=\"[r]\"", "Alias=\"[foreign]\""),
        ("NodeId=\"5\" PhysicalOp", "NodeId=\"5\" EstimateRowsWithoutRowGoal=\"32768\" PhysicalOp"),
        ("Parallel=\"false\"", "Parallel=\"true\""),
    ] {
        let altered = changed(&b, before, after);
        assert!(observed(&diff(&a, "Cached", &altered, "Scan")).is_none(), "{before} -> {after}");
    }
    for cache in ["RetrievedFromCache=\"false\"", "UnknownCacheMetadata=\"true\""] {
        let unconfirmed_reuse = changed(&a, "RetrievedFromCache=\"true\"", cache);
        assert!(observed(&diff(&unconfirmed_reuse, "Lookup", &b, "Scan")).is_none(), "{cache}");
    }
    let changed_input = changed(&b, "ParameterRuntimeValue=\"(314159)\"", "ParameterRuntimeValue=\"(314160)\"");
    assert_eq!(compare(&a, &changed_input).relation(), Relation::Different);
    assert!(observed(&diff(&a, "Cached", &changed_input, "Scan")).is_none());
}

#[test]
fn integer_runtime_inputs_keep_native_ranges_and_precision_beyond_binary64() {
    for (data_type, runtime, equivalent) in [
        ("tinyint", "255", "(+0255)"),
        ("smallint", "-32768", "(-032768)"),
        ("int", "-2147483648", "(-02147483648)"),
        ("bigint", "9223372036854775807", "(+9223372036854775807)"),
    ] {
        let a =
            changed(&native(runtime, true), "ParameterDataType=\"int\"", &format!("ParameterDataType=\"{data_type}\""));
        let b = changed(
            &native(equivalent, false),
            "ParameterDataType=\"int\"",
            &format!("ParameterDataType=\"{data_type}\""),
        );
        assert_eq!(compare(&a, &b).relation(), Relation::Same, "{data_type}");
    }
    let a = changed(&native("9223372036854775806", true), "ParameterDataType=\"int\"", "ParameterDataType=\"bigint\"");
    let b = changed(&native("9223372036854775807", false), "ParameterDataType=\"int\"", "ParameterDataType=\"bigint\"");
    assert_eq!(compare(&a, &b).relation(), Relation::Different);
}

#[test]
fn a_complete_parameter_population_is_bounded_without_accepting_its_prefix_or_unknown_uses() {
    let original = native("(1)", true);
    for count in [MAX_PLAN_PARAMETERS, MAX_PLAN_PARAMETERS + 1] {
        let additions = (1..count)
            .map(|index| {
                format!(r#"<ColumnReference Column="@p{index}" ParameterDataType="int" ParameterRuntimeValue="(1)"/>"#)
            })
            .collect::<String>();
        let a = changed(&original, "</ParameterList>", &format!("{additions}</ParameterList>"));
        let b = changed(&a, "RetrievedFromCache=\"true\"", "RetrievedFromCache=\"false\"");
        assert_eq!(
            compare(&a, &b).relation(),
            if count == MAX_PLAN_PARAMETERS { Relation::Same } else { Relation::Unavailable }
        );
    }
    let (a, b) = measured_pair();
    let unknown = changed(
        &b,
        r#"<ColumnReference Column="@p"/></Identifier>"#,
        r#"<ColumnReference Column="@unknown"/></Identifier>"#,
    );
    assert_eq!(compare(&a, &unknown).relation(), Relation::Unavailable);
}
