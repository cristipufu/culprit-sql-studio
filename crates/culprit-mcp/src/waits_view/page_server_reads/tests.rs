use super::*;
use culprit_core::{SourceOutcome, SourceState, telemetry::*};

fn data(count: usize) -> Box<Report> {
    let mut data: Box<Report> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    data.page_server_reads = None;
    let workload = data.snapshot.workload.as_mut().unwrap();
    workload.source = "private-source-password-canary".into();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.id = "private-query-display-canary".into();
    query.text = "SELECT 'private-sql-password-canary'".into();
    let native_query = NativeQueryIdentity {
        query_id: "42".into(),
        qualifiers: [("private".into(), "private-query-qualifier-canary".into())].into(),
    };
    query.native_identity = Some(native_query.clone());
    let prototype = query.plans[0].clone();
    query.plans = (0..count)
        .map(|index| {
            let mut plan = prototype.clone();
            plan.id = (index + 4).to_string();
            plan.native_identity = Some(NativePlanIdentity {
                plan_id: plan.id.clone(),
                query: native_query.clone(),
                qualifiers: [("private".into(), "private-plan-qualifier-canary".into())].into(),
            });
            plan.page_server_reads = Some(Box::new(PageServerReadSummary::NativeFragment {
                native: Some(
                    NativeIdentity::new(format!("private-row-canary-{index}"))
                        .qualified("plan_id", &plan.id)
                        .qualified("private", "private-native-scope-canary"),
                ),
                executions: "9007199254740993".into(),
                statistics: Box::new(PageServerReadStatistics {
                    availability: PageServerReadAvailability::Available,
                    average_pages: Some(12.5),
                    last_pages: Some("9007199254740993".into()),
                    minimum_pages: Some("0".into()),
                    maximum_pages: None,
                    standard_deviation_pages: None,
                }),
            }));
            plan
        })
        .collect();
    data.snapshot.sources = vec![SourceOutcome {
        source: culprit_core::source_id::QUERY_STORE.into(),
        state: SourceState::Partial,
        reason: Some("private-native-error-canary".into()),
        reason_message: None,
    }];
    data
}

fn compact(data: &Report) -> Value {
    let mut response = json!({});
    attach(&mut response, data);
    response
}

#[test]
fn compact_output_reanalyzes_raw_facts_bounds_entries_and_keeps_exact_text_without_numeric_placeholders() {
    let data = data(8);
    let value = super::super::report(&data);
    let view = &value["page_server_reads"];
    assert_eq!(view["entries_retained"], 8);
    assert_eq!(view["observed_rows"], 8);
    assert_eq!(view["entries_presented"], ENTRY_LIMIT);
    assert_eq!(view["entries_limited"], true);
    assert_eq!(view["partial"], true);
    assert_eq!(view["read_complete"], false);
    assert!(view["selected_rows"].is_null(), "scan history does not invent a selected archive-row count");
    assert_eq!(view["entries"][0]["executions"], "9007199254740993");
    let metrics = view["entries"][0]["metrics"].as_array().unwrap();
    assert!(metrics.iter().any(|metric| metric["text"] == "9007199254740993"));
    assert!(
        metrics.iter().all(|metric| metric.get("value").is_none()),
        "generic Metric.value placeholders are not native counts"
    );
    assert_ne!(view["entries"][0]["reference"], view["entries"][1]["reference"]);
    let encoded = serde_json::to_string(view).unwrap();
    for private in [
        "private-source",
        "private-query",
        "private-plan",
        "private-row",
        "private-native",
        "private-sql",
        "sql_handle",
        "qualifiers",
        "manifest_sha256",
        "payload_sha256",
    ] {
        assert!(!encoded.contains(private), "{private}");
    }
    assert!(encoded.len() < 20_000);
}

#[test]
fn entry_references_are_historical_and_capture_specific_without_native_proof_disclosure() {
    let mut data = data(1);
    let first = compact(&data);
    assert_eq!(first, compact(&data));
    let reference = first["page_server_reads"]["entries"][0]["reference"].clone();
    assert_eq!(reference.as_str().unwrap().len(), 64);
    data.snapshot.id = "a-different-original-scan".into();
    assert_ne!(reference, compact(&data)["page_server_reads"]["entries"][0]["reference"]);
}

#[test]
fn native_null_zero_and_non_applicability_do_not_change_the_original_read_outcome_or_become_equivalent() {
    let mut data = data(1);
    data.snapshot.sources[0].state = SourceState::Collected;
    for (availability, last, metric_count) in [
        (PageServerReadAvailability::Available, None, 5),
        (PageServerReadAvailability::Available, Some("0"), 5),
        (PageServerReadAvailability::NotApplicable, None, 0),
    ] {
        let PageServerReadSummary::NativeFragment { statistics, .. } =
            data.snapshot.workload.as_mut().unwrap().queries[0].plans[0].page_server_reads.as_mut().unwrap().as_mut()
        else {
            panic!()
        };
        **statistics =
            PageServerReadStatistics { availability, last_pages: last.map(Into::into), ..Default::default() };
        let value = compact(&data);
        let view = &value["page_server_reads"];
        assert_eq!(view["read_complete"], true);
        assert_eq!(view["partial"], false);
        let metrics = view["entries"][0]["metrics"].as_array().unwrap();
        assert_eq!(metrics.len(), metric_count);
        if availability == PageServerReadAvailability::Available {
            let metric = metrics.iter().find(|metric| metric["key"] == "view.page_server_reads.metric.last").unwrap();
            let expected =
                last.map(str::to_owned).unwrap_or_else(|| culprit_core::t!("view.page_server_reads.unknown"));
            assert_eq!(metric["text"], expected);
        }
    }
}

#[test]
fn stale_derived_report_cannot_replace_missing_or_invalid_native_facts() {
    let mut data = data(1);
    let mut stale = culprit_engine::explain_page_server_reads(&data.snapshot).unwrap().unwrap();
    stale.summary = "STALE-PRIVATE-DERIVED-CANARY".into();
    data.page_server_reads = Some(Box::new(stale));
    let PageServerReadSummary::NativeFragment { statistics, .. } =
        data.snapshot.workload.as_mut().unwrap().queries[0].plans[0].page_server_reads.as_mut().unwrap().as_mut()
    else {
        panic!()
    };
    statistics.last_pages = Some("-1".into());
    let value = compact(&data);
    assert!(value.get("page_server_reads").is_none());
    assert_eq!(value["page_server_reads_unavailable"]["raw_evidence_present"], true);
    assert!(!serde_json::to_string(&value).unwrap().contains("STALE-PRIVATE"));
    data.snapshot.workload.as_mut().unwrap().queries[0].plans[0].page_server_reads = None;
    let value = compact(&data);
    assert_eq!(value["page_server_reads_unavailable"]["raw_evidence_present"], false);
    assert!(value.get("page_server_reads").is_none());
}

#[test]
fn sharing_then_reanalysis_preserves_exact_facts_and_an_invalid_body_marker_without_leaking_native_metadata() {
    let mut data = data(1);
    data.snapshot = culprit_core::redact::redact_snapshot(&data.snapshot);
    let value = compact(&data);
    assert_eq!(value["page_server_reads"]["entries"][0]["executions"], "9007199254740993");
    assert!(!serde_json::to_string(&value).unwrap().contains("private-"));
    let PageServerReadSummary::NativeFragment { statistics, .. } =
        data.snapshot.workload.as_mut().unwrap().queries[0].plans[0].page_server_reads.as_mut().unwrap().as_mut()
    else {
        panic!()
    };
    statistics.last_pages = Some("-1".into());
    data.snapshot = culprit_core::redact::redact_snapshot(&data.snapshot);
    data.page_server_reads = None;
    let value = compact(&data);
    assert!(value.get("page_server_reads").is_none());
    assert_eq!(value["page_server_reads_unavailable"]["raw_evidence_present"], false);
    assert_eq!(value["page_server_reads_unavailable"]["read_complete"], false);
}

#[test]
fn unrelated_query_store_gaps_do_not_create_a_page_server_measurement_or_unavailable_claim() {
    let mut data = data(1);
    data.snapshot.workload.as_mut().unwrap().queries[0].plans[0].page_server_reads = None;
    assert_eq!(compact(&data), json!({}));
}

#[test]
fn an_absent_original_definition_carrier_is_a_gap_and_never_stale_derived_sql_authority() {
    use culprit_core::query_store_statement_report::*;
    let mut data = data(1);
    let mut stale = culprit_engine::explain_page_server_reads(&data.snapshot).unwrap().unwrap();
    stale.definition_read = Some(Box::new(QueryStoreDefinitionRead {
        namespace: QueryStoreDefinitionNamespace::Matched,
        selected_rows: 1,
        observed_rows: 1,
        read_complete: true,
    }));
    stale.entries[0].statement = Some(Box::new(QueryStoreStatementExplanation {
        binding: QueryStoreStatementBinding::Matched,
        definition: None,
        origin: StatementOrigin::CulpritMonitoring,
        monitoring_template_id: Some(digest(b"private-stale-template-canary")),
        preview: Some("SELECT 'private-stale-statement-canary'".into()),
        preview_truncated: false,
        unavailable: None,
        unavailable_explanation: None,
        explanation: "private-stale-binding-canary".into(),
    }));
    data.page_server_reads = Some(Box::new(stale));
    let value = compact(&data);
    let view = &value["page_server_reads"];
    assert_eq!(view["definition_evidence_unavailable"], true);
    assert!(view["definition_read"].is_null());
    assert!(view["entries"][0]["statement_binding"].is_null());
    assert_eq!(view["entries"][0]["executions"], "9007199254740993");
    let encoded = serde_json::to_string(&value).unwrap();
    for secret in [
        "private-stale-statement",
        "private-stale-template",
        "private-stale-binding",
        "monitoring_template_id",
        "preview",
    ] {
        assert!(!encoded.contains(secret), "{secret}");
    }
}
