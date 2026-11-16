use super::*;

fn parsed(parallel: &str, counters: &str, operator: &str) -> culprit_core::Plan {
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
    <StmtSimple StatementText="SELECT * FROM @rows" QueryHash="0xA" StatementSubTreeCost="1"><QueryPlan>
    <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1" EstimatedRowsRead="1" {parallel} EstimatedTotalSubtreeCost="1">
    <RunTimeInformation>{counters}</RunTimeInformation>{operator}</RelOp>
    </QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    crate::plan_xml::parse_statement(&xml, Some("0xA")).unwrap()
}

const TABLE: &str = r#"<TableScan><Object Table="[@rows]" /></TableScan>"#;

#[test]
fn serial_execution_counts_require_explicit_serial_native_scope_and_preserve_exact_values() {
    for value in ["1", "10000", "9007199254740993", "18446744073709551615"] {
        let counters = format!(r#"<RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="{value}" />"#);
        let plan = parsed(r#"Parallel="0""#, &counters, TABLE);
        assert_eq!(plan.root.facts().serial_execution_count.as_deref(), Some(value));
    }
    let counter = r#"<RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" />"#;
    for (parallel, counters) in [
        ("", counter.to_owned()),
        (r#"Parallel="unknown""#, counter.to_owned()),
        (r#"Parallel="1""#, counter.to_owned()),
        (r#"Parallel="1""#, counter.replace("Thread=\"0\"", "Thread=\"1\"")),
        (r#"Parallel="0""#, counter.replace("Thread=\"0\"", "Thread=\"1\"")),
        (r#"Parallel="0""#, counter.repeat(2)),
        (r#"Parallel="0""#, counter.replace("ActualRows=\"1\"", "")),
        (r#"Parallel="0""#, counter.replace("ActualRows=\"1\"", "ActualRows=\"NaN\"")),
        (r#"Parallel="0""#, counter.replace("ActualExecutions=\"1\"", "ActualExecutions=\"0\"")),
        (r#"Parallel="0""#, counter.replace("ActualExecutions=\"1\"", "ActualExecutions=\"1.5\"")),
    ] {
        let plan = parsed(parallel, &counters, TABLE);
        assert!(plan.root.facts().serial_execution_count.is_none(), "{parallel} {counters}");
    }
}

#[test]
fn duplicate_runtime_populations_cannot_authorize_serial_scope_even_when_the_first_one_looks_valid() {
    let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple QueryHash="0xA"><QueryPlan>
        <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="0" EstimateRows="1">
        <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation>
        <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation>
        <TableScan><Object Table="[@rows]" /></TableScan></RelOp>
        </QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
    let plan = crate::plan_xml::parse_statement(xml, Some("0xA")).unwrap();
    assert!(plan.root.facts().serial_execution_count.is_none());
    assert!(plan.root.facts().scan_completion.is_none());
    assert_eq!(plan.root.act_rows, Some(1.0));
}

#[test]
fn native_eof_counts_preserve_zero_missing_and_large_integers_without_float_or_property_fallbacks() {
    for value in [None, Some("0"), Some("1"), Some("9007199254740993"), Some("18446744073709551615")] {
        let eof = value.map(|value| format!(r#" ActualEndOfScans="{value}""#)).unwrap_or_default();
        let native = format!(
            r#"<RunTimeCountersPerThread Thread="0" ActualRows="1" ActualRowsRead="1" ActualExecutions="1"{eof} />"#
        );
        let plan = parsed(r#"Parallel="0""#, &native, TABLE);
        let completion = plan.root.facts().scan_completion.as_ref().unwrap();
        assert_eq!(completion.threads[0].end_of_scans.as_deref(), value);
        assert_eq!(completion.is_single_completed_serial_scan(), value == Some("1"));
    }
    let mut plan = parsed(
        r#"Parallel="0""#,
        r#"<RunTimeCountersPerThread Thread="0" ActualRows="1" ActualRowsRead="1" ActualExecutions="1" />"#,
        TABLE,
    );
    plan.root.props.insert("ActualEndOfScans".into(), "1".into());
    assert!(!plan.root.facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
}

#[test]
fn completion_uses_the_original_worker_population_and_never_relabels_other_access_paths() {
    let counters = r#"<RunTimeCountersPerThread Thread="0" ActualRows="0" ActualExecutions="0" ActualEndOfScans="0" />
        <RunTimeCountersPerThread Thread="1" ActualRows="5" ActualRowsRead="5" ActualExecutions="1" ActualEndOfScans="1" />
        <RunTimeCountersPerThread Thread="2" ActualRows="5" ActualRowsRead="5" ActualExecutions="1" ActualEndOfScans="1" />"#;
    let plan = parsed(r#"Parallel="1""#, counters, TABLE);
    let observed = plan.root.facts().scan_completion.as_ref().unwrap();
    assert_eq!(observed.threads.len(), 3);
    assert_eq!(observed.threads[0].end_of_scans.as_deref(), Some("0"));
    assert!(!observed.is_single_completed_serial_scan());
    for operator in [r#"<IndexScan><Object Table="[@rows]" Index="[ix]" /></IndexScan>"#, r#"<TableSpool />"#] {
        assert!(parsed(r#"Parallel="0""#, counters, operator).root.facts().scan_completion.is_none());
    }
}

#[test]
fn malformed_duplicate_or_oversized_worker_metadata_withholds_completion_without_losing_rows() {
    let original = r#"<RunTimeCountersPerThread Thread="0" ActualRows="1" ActualRowsRead="1" ActualExecutions="1" ActualEndOfScans="1" />"#;
    for counters in [
        original.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"-1\""),
        original.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"1.0\""),
        original.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"18446744073709551616\""),
        original.replace("Thread=\"0\"", "Thread=\"-1\""),
        format!("{original}{original}"),
    ] {
        let plan = parsed(r#"Parallel="0""#, &counters, TABLE);
        assert!(plan.root.facts().scan_completion.is_none(), "{counters}");
        assert!(plan.root.act_rows.is_some());
    }
    let counters = (0..=MAX_PLAN_SCAN_THREADS)
        .map(|thread| original.replace("Thread=\"0\"", &format!("Thread=\"{thread}\"")))
        .collect::<String>();
    assert!(parsed(r#"Parallel="1""#, &counters, TABLE).root.facts().scan_completion.is_none());
}
