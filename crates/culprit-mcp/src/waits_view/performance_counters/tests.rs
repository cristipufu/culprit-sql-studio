use super::*;
use culprit_core::{
    MeasurementScope, Metric, PerformanceCounterAvailability, PerformanceCounterEntry, PerformanceCounterReport,
    ResetMarker, SourceState, telemetry::*,
};

fn saved_report() -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    report.performance_counters = vec![PerformanceCounterReport {
        population: culprit_core::PerformanceCounterPopulation::SharedServer,
        native_database_id: None,
        database: None,
        context: None,
        native_source: None,
        source_state: Some(SourceState::Partial),
        source_reason: Some("private-native-failure-transcript".into()),
        read_complete: Some(false),
        native_observations: Some(1),
        observed_observations: 1,
        native_counter_rows: Some(12),
        observed_counter_rows: 12,
        presented_counter_rows: 12,
        entries_limited: false,
        fields_withheld: false,
        missing_fields: vec![Metric::text("missing field", "unknown")],
        title: "counter context".into(),
        summary: "partial".into(),
        limitations: vec!["shared server, no owner".into()],
        ratios: None,
        entries: (0..12)
            .map(|index| PerformanceCounterEntry {
                resource_governor: None,
                observation_key: "private-native-parent".into(),
                counter_key: format!("private-native-key-{index}"),
                field: "granted_workspace".into(),
                family: PerformanceCounterFamily::WorkspaceMemory,
                family_label: "workspace".into(),
                availability: PerformanceCounterAvailability::Observed,
                native_object: "private-native-object".into(),
                native_name: "private-native-name".into(),
                native_instance: Some("private-native-instance".into()),
                native_type: Some("65792".into()),
                observed_at: Some("2020-01-01T12:00:00Z".into()),
                scope: MeasurementScope::Server,
                nature: CounterNature::Gauge,
                unit: CounterUnit::Kibibytes,
                reset: ResetMarker::Unknown,
                raw_value: "9007199254740993".into(),
                metric: Metric::text("workspace", "9007199254740993 KiB"),
                explanation: "not pressure".into(),
            })
            .collect(),
    }];
    report
}

#[test]
fn compact_output_keeps_counts_and_exact_display_text_without_native_labels_or_raw_properties() {
    let mut report = saved_report();
    let mut response = json!({});
    attach(&mut response, &report);
    let counters = &response["performance_counters"][0];
    assert_eq!(counters["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(counters["entries_limited"], true);
    assert_eq!(counters["native_counter_rows"], 12);
    assert_eq!(counters["presented_counter_rows"], 12);
    assert_eq!(counters["returned_counter_rows"], ENTRY_LIMIT);
    assert_eq!(counters["read_complete"], false);
    let text = serde_json::to_string(&response).unwrap();
    assert!(text.contains("9007199254740993 KiB"));
    assert!(!text.contains("private-native"));
    assert!(!text.contains("native_type"));
    report.performance_counters.clear();
    report.snapshot.record_unavailable_evidence(
        culprit_core::source_id::PERFORMANCE_COUNTERS,
        culprit_core::message!("view.performance_counters.invalid"),
    );
    let mut response = json!({});
    attach(&mut response, &report);
    assert!(response.get("performance_counters_unavailable").is_some());
    assert!(response.get("performance_counters").is_none());
}

#[test]
fn saved_counter_collection_keeps_independent_qualifications_and_withdrawn_selected_scope() {
    let mut report = saved_report();
    let mut selected = report.performance_counters[0].clone();
    selected.population = culprit_core::PerformanceCounterPopulation::SelectedDatabase;
    selected.source_state = Some(SourceState::Unavailable);
    selected.summary = culprit_core::t!("view.performance_counters.sharing_withheld");
    selected.source_reason = Some(selected.summary.clone());
    selected.native_observations = None;
    selected.native_counter_rows = None;
    selected.observed_observations = 0;
    selected.observed_counter_rows = 0;
    selected.presented_counter_rows = 0;
    selected.entries.clear();
    report.performance_counters.push(selected);
    let mut response = json!({});
    attach(&mut response, &report);
    let populations = response["performance_counters"].as_array().unwrap();
    assert_eq!(populations.len(), 2);
    assert_eq!(populations[0]["population"], "shared-server");
    assert_eq!(populations[0]["source_state"], "partial");
    assert_eq!(populations[0]["native_counter_rows"], 12);
    assert_eq!(populations[1]["population"], "selected-database");
    assert_eq!(populations[1]["source_state"], "unavailable");
    for field in ["database", "native_database_id", "read_bounds", "native_counter_rows"] {
        assert!(populations[1][field].is_null(), "{field}");
    }
    assert!(populations[1]["entries"].as_array().unwrap().is_empty());
    assert!(!serde_json::to_string(&response).unwrap().contains("private-native"));
}

#[test]
fn selected_report_read_bounds_survive_compact_projection_without_native_owner_proof() {
    let mut report = saved_report();
    let counters = &mut report.performance_counters[0];
    counters.population = culprit_core::PerformanceCounterPopulation::SelectedDatabase;
    counters.database = Some("original-database".into());
    counters.native_database_id = Some("7".into());
    counters.context = Some(ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "original-database",
                "private-native-host",
                1433,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: Some(true),
        }),
    });
    let value = value(counters);
    assert_eq!(value["population"], "selected-database");
    assert_eq!(value["database"], "original-database");
    assert_eq!(value["read_bounds"]["observed_from"], "2020-01-01T12:00:00Z");
    assert_eq!(value["read_bounds"]["observed_until"], "2020-01-01T12:00:01Z");
    assert!(!serde_json::to_string(&value).unwrap().contains("private-native"));
}

#[test]
fn compact_common_counters_include_the_canonical_fraction_without_raw_native_labels_or_receipts() {
    use culprit_collector::analytical::test_support::{
        CommonPerformanceCounterCase as Case, common_performance_counter_evidence,
    };
    for case in Case::ALL {
        let (source, records) = common_performance_counter_evidence(case);
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let rows = records
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                (row.source == culprit_core::source_id::PERFORMANCE_COUNTERS).then_some(row)
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::performance_counters::explain_source(&source, coverage, &rows, true).unwrap();
        let original = serde_json::to_value(&report).unwrap();
        let result = value(&report);
        assert_eq!(result["source_state"], serde_json::to_value(coverage.state).unwrap());
        assert_eq!(result["population"], "shared-server");
        assert_eq!(result["returned_counter_rows"], report.entries.len().min(ENTRY_LIMIT));
        assert!(result["entries"].as_array().unwrap().len() <= ENTRY_LIMIT);
        assert!(result["database"].is_null() && result["native_database_id"].is_null());
        if case == Case::Denied {
            assert!(result["ratios"].is_null());
            assert!(result["entries"].as_array().unwrap().is_empty());
        } else {
            let fraction = result["ratios"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["kind"] == "buffer-cache-hit")
                .unwrap();
            assert_eq!(fraction["scope"], "server");
            let metrics = fraction["metrics"].as_array().unwrap();
            let percent = metrics.iter().find(|metric| metric["key"] == "view.counter_ratios.metric.percent");
            assert_eq!(
                percent.is_some(),
                !matches!(case, Case::MeasuredZero | Case::MissingBase | Case::MissingCommon)
            );
            assert_eq!(fraction["observed_at"], "2020-01-01T12:00:00.500000000Z");
        }
        let text = serde_json::to_string(&result).unwrap();
        for private in
            ["SQLServer:", "native_type", "native_object", "resource_governor_instance_epoch", "native-instance-a"]
        {
            assert!(!text.contains(private), "{case:?}: {private}");
        }
        assert_eq!(serde_json::to_value(&report).unwrap(), original);
    }
}
