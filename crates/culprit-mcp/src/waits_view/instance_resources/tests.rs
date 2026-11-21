use super::*;
use culprit_core::{
    InstanceResourceReport, SchedulerEntry, SchedulerField, SchedulerFieldAvailability, SchedulerReport, SourceState,
    telemetry::*,
};

fn report() -> InstanceResourceReport {
    let field = SchedulerField {
        native_name: "work_queue_count".into(),
        label: "native field".into(),
        value: Some("9007199254740993".into()),
        unqualified: None,
        availability: SchedulerFieldAvailability::Observed,
        explanation: None,
        unit: Some(CounterUnit::Count),
        nature: Some(CounterNature::Gauge),
        reset: Some(culprit_core::ResetMarker::Unknown),
    };
    InstanceResourceReport {
        numa_nodes: None,
        process_cpu: None,
        context: None,
        native_source: None,
        source_state: SourceState::Unavailable,
        read_complete: None,
        observed_at: None,
        native_populations_complete: None,
        title: "resource title".into(),
        summary: "resource summary".into(),
        limitations: vec![],
        groups: vec![],
        schedulers: Some(Box::new(SchedulerReport {
            scope: culprit_core::MeasurementScope::Server,
            population: "population".into(),
            context: Some(ObservationReadContext::Collector {
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
            }),
            native_source: Some(StateSourceCoverage::observed(culprit_core::source_id::SCHEDULERS.into(), 9, false)),
            source_state: SourceState::Partial,
            source_reason: None,
            read_complete: Some(false),
            native_rows: Some(9),
            observed_rows: 7,
            presented_rows: 7,
            entries_limited: false,
            title: "scheduler title".into(),
            summary: "scheduler summary".into(),
            limitations: vec!["qualification".into()],
            entries: (0..7)
                .map(|id| SchedulerEntry {
                    observation_key: id.to_string(),
                    scheduler_id: id.to_string(),
                    title: format!("scheduler {id}"),
                    fields: vec![field.clone()],
                })
                .collect(),
        })),
    }
}

#[test]
fn compact_scheduler_values_preserve_exact_inputs_typed_unknowns_and_original_counts_without_raw_context() {
    let mut report = report();
    let fields = &mut report.schedulers.as_mut().unwrap().entries[0].fields;
    fields.push(SchedulerField {
        native_name: "failed_to_create_worker".into(),
        label: "native flag".into(),
        value: None,
        unqualified: None,
        availability: SchedulerFieldAvailability::NativeNull,
        explanation: None,
        unit: None,
        nature: None,
        reset: None,
    });
    fields.push(SchedulerField {
        native_name: "ideal_workers_limit".into(),
        label: "native limit".into(),
        value: None,
        unqualified: None,
        availability: SchedulerFieldAvailability::ColumnUnavailable,
        explanation: None,
        unit: Some(CounterUnit::Count),
        nature: Some(CounterNature::Gauge),
        reset: Some(culprit_core::ResetMarker::Unknown),
    });
    let value = value(&report);
    let schedulers = &value["schedulers"];
    assert_eq!(schedulers["scope"], "server");
    assert_eq!(schedulers["source_state"], "partial");
    assert_eq!(schedulers["native_rows"], 9);
    assert_eq!(schedulers["observed_rows"], 7);
    assert_eq!(schedulers["entries_presented"], 5);
    assert_eq!(schedulers["entries_omitted"], 2);
    assert_eq!(schedulers["entries"][0]["fields"][0]["value"], "9007199254740993");
    assert_eq!(schedulers["entries"][0]["fields"][0]["nature"], "gauge");
    assert_eq!(schedulers["entries"][0]["fields"][0]["reset"]["kind"], "unknown");
    assert_eq!(schedulers["entries"][0]["fields"][1]["availability"], "native-null");
    assert_eq!(schedulers["entries"][0]["fields"][2]["availability"], "column-unavailable");
    assert!(schedulers["entries"][0]["fields"][1]["value"].is_null());
    assert_eq!(schedulers["original_read_bounds"]["until"], "2020-01-01T00:00:01Z");
    assert!(schedulers.get("context").is_none());
}

#[test]
fn compact_scheduler_formatter_bounds_extra_fields_and_native_strings_independently() {
    let mut report = report();
    let schedulers = report.schedulers.as_mut().unwrap();
    schedulers.title = "é".repeat(4096);
    let entry = &mut schedulers.entries[0];
    let mut field = entry.fields[0].clone();
    field.value = Some("x".repeat(4096));
    field.label = "y".repeat(4096);
    entry.fields = vec![field; 100];
    let value = value(&report);
    let schedulers = &value["schedulers"];
    assert_eq!(schedulers["title"].as_str().unwrap().chars().count(), 512);
    let entry = &schedulers["entries"][0];
    assert_eq!(entry["fields_presented"], 29);
    assert_eq!(entry["fields_omitted"], 71);
    assert_eq!(entry["fields"][0]["value"].as_str().unwrap().chars().count(), 512);
    assert_eq!(entry["fields"][0]["label"].as_str().unwrap().chars().count(), 512);
}

#[test]
fn compact_spinlock_diagnostics_keep_the_raw_value_but_withhold_the_measurement_slot() {
    let mut report = report();
    let mut field = report.schedulers.as_ref().unwrap().entries[0].fields[0].clone();
    field.native_name = "spinlock_wait_count".into();
    field.value = None;
    field.availability = SchedulerFieldAvailability::InstrumentationUnqualified;
    field.unqualified =
        Some(culprit_core::SchedulerUnqualifiedValue { native_value: "0".into(), required_global_trace_flag: 8134 });
    report.schedulers.as_mut().unwrap().entries[0].fields.push(field);
    let value = value(&report);
    let field = &value["schedulers"]["entries"][0]["fields"][1];
    assert!(field["value"].is_null());
    assert_eq!(field["availability"], "instrumentation-unqualified");
    assert_eq!(field["unqualified"]["native_value"], "0");
    assert_eq!(field["unqualified"]["required_global_trace_flag"], 8134);
}

#[test]
fn original_numa_child_preserves_independent_coverage_bounds_nulls_and_bounded_native_fields() {
    use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
    for case in [NumaNodeCase::Exact, NumaNodeCase::NativeNull, NumaNodeCase::CpuDenied, NumaNodeCase::Limited] {
        let (source, rows) = numa_node_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                culprit_core::telemetry::TelemetryRecord::Operational(row) => row,
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
        let value = super::value(&report);
        let nodes = &value["numa_nodes"];
        assert_eq!(nodes["scope"], "server");
        assert_eq!(nodes["populations"].as_array().unwrap().len(), 2);
        assert_eq!(nodes["original_read_bounds"]["from"], source.observation.snapshot().unwrap().observed_from);
        for population in nodes["populations"].as_array().unwrap() {
            assert!(population["entries"].as_array().unwrap().len() <= 5);
            for entry in population["entries"].as_array().unwrap() {
                assert!(entry["fields"].as_array().unwrap().len() <= 13);
            }
        }
        for private in ["source_sha256", "original-instance", "manifest_sha256"] {
            assert!(!value.to_string().contains(private));
        }
    }
}
