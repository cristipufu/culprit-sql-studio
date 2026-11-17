use super::*;
use culprit_core::{Metric, page_server_read_report::*, telemetry::*};

fn report() -> PageServerReadReport {
    let entry = PageServerReadEntry {
        identity: PageServerReadEntryIdentity::RecordedPlan {
            query_label: "display-query".into(),
            plan_label: "display-plan".into(),
            native_plan: Some(Box::new(NativePlanIdentity {
                plan_id: "4".into(),
                query: NativeQueryIdentity { query_id: "42".into(), qualifiers: Default::default() },
                qualifiers: Default::default(),
            })),
            native_fragment: Some(Box::new(NativeIdentity::new("17"))),
            native_fragments: "1".into(),
            execution_type: Some("0".into()),
        },
        executions: "9007199254740993".into(),
        statement: None,
        explanation: PageServerReadExplanation {
            availability: PageServerReadAvailability::Available,
            title: "Retained native fragment".into(),
            summary: t!("view.page_server_reads.observed"),
            metrics: vec![
                Metric::said_text("view.page_server_reads.metric.last", "9007199254740993"),
                Metric::said_text("view.page_server_reads.metric.minimum", "0"),
                Metric::said_text("view.page_server_reads.metric.maximum", t!("view.page_server_reads.unknown")),
            ],
            limitations: vec![t!("view.page_server_reads.limit.origin"), t!("view.page_server_reads.limit.population")],
            investigate: t!("view.page_server_reads.investigate"),
            verify: t!("view.page_server_reads.verify"),
        },
    };
    PageServerReadReport {
        source: "native-history".into(),
        availability: Some(PageServerReadAvailability::Available),
        read_complete: false,
        selected_rows: Some(9),
        observed_rows: 8,
        presentation_limited: false,
        source_state: Some(SourceState::Partial),
        source_reason: None,
        title: t!("view.page_server_reads.title"),
        summary: t!("view.page_server_reads.summary.archive", observed = "8", selected = "9"),
        limitations: vec![
            t!("view.page_server_reads.limit.lifetime"),
            t!("view.page_server_reads.limit.storage"),
            t!("view.page_server_reads.limit.population"),
            t!("view.page_server_reads.limit.precision"),
            t!("view.page_server_reads.limit.origin"),
            t!("view.page_server_reads.limit.read"),
        ],
        definition_read: None,
        runtime_read: None,
        reference_coverage: None,
        entries: (0..8)
            .map(|index| {
                let mut copy = entry.clone();
                copy.explanation.title = format!("Native fragment {index}");
                copy
            })
            .collect(),
    }
}

#[test]
fn text_keeps_exact_values_every_qualification_and_bounded_historical_context() {
    let report = report();
    let text = page_server_reads(&report);
    assert!(text.contains("9007199254740993") && !text.contains("9007199254740992"));
    assert!(text.contains(&t!("view.page_server_reads.unknown")));
    for qualification in &report.limitations {
        assert!(text.contains(qualification));
    }
    assert!(text.contains(&t!("evidence.cli.source_partial")));
    assert!(text.contains(&t!("evidence.cli.page_server_reads.reference", query = "42", plan = "4", row = "17")));
    let printed = super::super::super::BRIEF_FINDING_LIMIT;
    for index in 0..printed {
        assert!(text.contains(&format!("Native fragment {index}")));
    }
    assert!(!text.contains(&format!("Native fragment {printed}")));
    assert!(text.contains(&t!(
        "evidence.cli.page_server_reads.counts",
        observed = "8",
        selected = "9",
        printed = printed.to_string()
    )));
    assert!(text.contains(&t!("view.page_server_reads.limit.definition_link")));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["entries"][0]["executions"], "9007199254740993");
    assert_eq!(json["selected_rows"], 9);
    assert_eq!(json["read_complete"], false);
}

#[test]
fn empty_unavailable_statistics_remain_qualified_without_counterfeit_numeric_observations() {
    let mut report = report();
    report.entries.clear();
    report.availability = Some(PageServerReadAvailability::Unavailable);
    report.selected_rows = Some(0);
    report.observed_rows = 0;
    report.read_complete = true;
    report.source_state = Some(SourceState::Empty);
    report.summary = t!("view.page_server_reads.summary.archive", observed = "0", selected = "0");
    report.limitations.push(t!("view.page_server_reads.unavailable"));
    let text = page_server_reads(&report);
    assert!(text.contains(&t!("view.page_server_reads.unavailable")));
    assert!(!text.contains("9007199254740993"));
    assert!(!text.contains(&t!("view.page_server_reads.metric.last")));
    assert!(text.contains(&t!("view.page_server_reads.limit.lifetime")));
}

#[test]
fn original_archive_fragment_renders_its_native_ids_without_definition_or_sql_lookup() {
    let case = culprit_collector::analytical::test_support::page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "native_fragment")
        .unwrap();
    let capture = CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let selected_rows =
        case.records.iter().filter(|record| record.stream() == TelemetryStream::QueryRuntime).count() as u32;
    let read = CapturedReadRef::new(
        &case.source,
        capture.clone(),
        digest(b"renderer-original-manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: selected_rows }],
    )
    .unwrap();
    // A fixture retains its own canonical export before issuing a receipt. The consumer receives those
    // unchanged bytes; it must never canonicalize an incoming body to make a mismatched proof fit.
    let records = case
        .records
        .iter()
        .filter(|record| record.stream() == TelemetryStream::QueryRuntime)
        .map(|record| ExportedRecord::new(&case.source, &capture, record.clone()).unwrap())
        .collect::<Vec<_>>();
    let rows = records
        .iter()
        .enumerate()
        .map(|(ordinal, exported)| {
            let TelemetryRecord::QueryRuntime(runtime) = &exported.record else { panic!() };
            culprit_checks::page_server_reads::RuntimeEvidence {
                reference: CapturedRecordReference::from_export(&read, &case.source, exported, ordinal as u32).unwrap(),
                runtime,
            }
        })
        .collect::<Vec<_>>();
    let reference_coverage = ReferenceCoverage {
        queries: DefinitionCoverage { required: selected_rows, resolved: 0, unresolved: selected_rows },
        plans: DefinitionCoverage { required: selected_rows, resolved: 0, unresolved: selected_rows },
        complete: false,
        integrity_complete: true,
        ..Default::default()
    };
    let report =
        culprit_checks::page_server_reads::explain_archive(culprit_checks::page_server_reads::PageServerReadEvidence {
            source: &case.source,
            read: &read,
            reference_coverage: &reference_coverage,
            rows: &rows,
            selected_rows,
            read_complete: true,
            definitions: None,
        })
        .unwrap();
    let text = page_server_reads(&report);
    assert!(matches!(report.entries[0].identity, PageServerReadEntryIdentity::RuntimeFragment { .. }));
    assert!(text.contains(&t!("evidence.cli.page_server_reads.reference", query = "42", plan = "4", row = "17")));
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.page_server_reads.limit.definition_link")));
    assert!(!text.contains("SELECT"));
}

#[test]
fn partial_unverified_statement_evidence_stays_visible_with_native_counts_and_no_invented_sql() {
    use culprit_core::query_store_statement_report::*;
    let mut report = report();
    report.definition_read = Some(Box::new(QueryStoreDefinitionRead {
        namespace: QueryStoreDefinitionNamespace::Unverified,
        selected_rows: 2,
        observed_rows: 1,
        read_complete: false,
    }));
    report.limitations.extend([
        t!("view.page_server_reads.statement.unverified"),
        t!("view.page_server_reads.statement.limit.partial"),
        t!("view.page_server_reads.statement.limit.continuity"),
    ]);
    report.entries[0].statement = Some(Box::new(QueryStoreStatementExplanation {
        binding: QueryStoreStatementBinding::SourceUnverified,
        definition: None,
        origin: StatementOrigin::Unknown,
        monitoring_template_id: None,
        preview: None,
        preview_truncated: false,
        unavailable: Some(PayloadUnavailable::NotCollected),
        unavailable_explanation: Some(t!(
            "view.query_store_statement.unavailable",
            reason = t!("engine.cached_plans.reason.not_collected")
        )),
        explanation: t!("view.page_server_reads.statement.unverified"),
    }));
    let text = page_server_reads(&report);
    for qualification in &report.limitations {
        assert!(text.contains(qualification), "{qualification}");
    }
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("evidence.cli.page_server_reads.definition_counts", observed = "1", selected = "2")));
    assert!(!text.contains(&t!("view.page_server_reads.limit.definition_link")));
    assert!(!text.contains("SELECT"));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["entries"][0]["statement"]["binding"], "source-unverified");
    assert!(json["entries"][0]["statement"]["preview"].is_null());
}
