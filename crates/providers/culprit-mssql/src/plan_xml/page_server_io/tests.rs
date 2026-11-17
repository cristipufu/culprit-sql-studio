use culprit_core::{Plan, PlanKind, actual::page_server_io::*};

const HASH: &str = "0x0123456789ABCDEF";

fn thread(id: &str, attributes: &str) -> String {
    format!(r#"<RunTimeCountersPerThread Thread="{id}" ActualRows="10" ActualExecutions="1" {attributes} />"#)
}
fn statement(hash: &str, runtime: &str) -> String {
    format!(
        r#"<StmtSimple QueryHash="{hash}" StatementText="SELECT value FROM dbo.orders"><QueryPlan>
        <RelOp NodeId="7" PhysicalOp="Index Scan" LogicalOp="Index Scan" EstimateRows="10">
        {runtime}<IndexScan><Object Schema="[dbo]" Table="[orders]" Index="[ix_orders]" /></IndexScan>
        </RelOp></QueryPlan></StmtSimple>"#
    )
}
fn xml(statements: &str) -> String {
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}
fn population(threads: &str) -> String {
    format!("<RunTimeInformation>{threads}</RunTimeInformation>")
}
fn plan(threads: &str) -> Plan {
    super::super::parse_native_statement(&xml(&statement(HASH, &population(threads))), Some(HASH)).unwrap()
}
fn io(plan: &Plan) -> &PlanPageServerIo {
    plan.root.runtime.as_ref().unwrap().page_server_io.as_ref().unwrap()
}

#[test]
fn original_native_attributes_remain_exact_independent_pages_on_the_selected_operator() {
    let actual = plan(&format!(
        "{}{}",
        thread(
            "1",
            r#"ActualPageServerReads="9007199254740993" ActualPageServerReadAheads="2" ActualLobPageServerReads="3" ActualLobPageServerReadAheads="0""#
        ),
        thread(
            "2",
            r#"ActualPageServerReads="2" ActualPageServerReadAheads="5" ActualLobPageServerReads="7" ActualLobPageServerReadAheads="0""#
        ),
    ));
    assert_eq!(actual.kind, PlanKind::Actual);
    let native = io(&actual);
    assert!(native.is_valid());
    assert_eq!(native.threads_observed, 2);
    assert_eq!(native.reads.as_deref(), Some("9007199254740995"));
    assert_eq!(native.read_aheads.as_deref(), Some("7"));
    assert_eq!(native.lob_reads.as_deref(), Some("10"));
    assert_eq!(native.lob_read_aheads.as_deref(), Some("0"));
    let runtime = actual.root.runtime.as_ref().unwrap();
    assert_eq!(runtime.logical_reads, None);
    assert_eq!(runtime.physical_reads, None, "remote pages do not become generic disk I/O");
    let summary = culprit_core::actual_summary(&actual).unwrap();
    assert_eq!(summary.logical_reads, None);
    assert_eq!(summary.physical_reads, None);
}

#[test]
fn absent_estimated_and_sibling_only_fields_do_not_manufacture_remote_zeroes_or_runtime() {
    let ordinary = plan(&thread("0", ""));
    assert!(ordinary.root.runtime.as_ref().is_none_or(|runtime| runtime.page_server_io.is_none()));
    let batch = xml(&format!(
        "{}{}",
        statement(HASH, ""),
        statement("0xFEDCBA9876543210", &population(&thread("0", r#"ActualPageServerReads="91""#))),
    ));
    let selected = super::super::parse_native_statement(&batch, Some(HASH)).unwrap();
    assert_eq!(selected.kind, PlanKind::Estimated);
    assert!(selected.root.runtime.is_none());
    assert!(super::super::parse_native_statement(&batch, Some("0x1111111111111111")).is_err());
    let duplicate = xml(&format!("{}{}", statement(HASH, ""), statement(HASH, &population(&thread("0", "")))));
    assert!(super::super::parse_native_statement(&duplicate, Some(HASH)).is_err());
}

#[test]
fn partial_and_malformed_thread_fields_preserve_independent_valid_counters() {
    for attributes in [
        r#"ActualPageServerReadAheads="0" ActualLobPageServerReads="2""#,
        r#"ActualPageServerReads="-1" ActualPageServerReadAheads="0" ActualLobPageServerReads="2""#,
    ] {
        let actual = plan(&format!(
            "{}{}",
            thread("1", attributes),
            thread("2", r#"ActualPageServerReads="5" ActualPageServerReadAheads="0" ActualLobPageServerReads="3""#),
        ));
        assert!(io(&actual).is_valid());
        assert_eq!(io(&actual).reads, None);
        assert_eq!(io(&actual).read_aheads.as_deref(), Some("0"));
        assert_eq!(io(&actual).lob_reads.as_deref(), Some("5"));
        assert_eq!(io(&actual).lob_read_aheads, None);
    }
}

#[test]
fn duplicate_thread_ids_and_multiple_runtime_populations_cannot_choose_a_representative() {
    let repeated = plan(&format!(
        "{}{}",
        thread("1", r#"ActualPageServerReads="1""#),
        thread("1", r#"ActualPageServerReads="2""#),
    ));
    assert!(io(&repeated).issues.contains(&PlanPageServerIoIssue::DuplicateThread));
    assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| io(&repeated).value(metric).is_none()));

    let populations = format!(
        "{}{}",
        population(&thread("0", r#"ActualPageServerReads="1""#)),
        population(&thread("1", r#"ActualPageServerReads="2""#)),
    );
    let actual = super::super::parse_native_statement(&xml(&statement(HASH, &populations)), Some(HASH)).unwrap();
    assert!(io(&actual).issues.contains(&PlanPageServerIoIssue::MultiplePopulations));
    assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| io(&actual).value(metric).is_none()));
}

#[test]
fn redaction_preserves_valid_exact_pages_but_discards_invalid_direct_saved_runtime() {
    let actual = plan(&thread("0", r#"ActualPageServerReads="9007199254740993" ActualPageServerReadAheads="0""#));
    let shared = culprit_core::redact::redact_plan(&actual);
    assert_eq!(io(&actual), io(&shared));
    assert!(shared.raw.is_none());
    let mut malformed = actual;
    malformed.root.runtime.as_mut().unwrap().page_server_io.as_mut().unwrap().reads =
        Some("private-saved-counter".into());
    let shared = culprit_core::redact::redact_plan(&malformed);
    assert!(shared.root.runtime.as_ref().unwrap().page_server_io.is_none());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-saved-counter"));
}
