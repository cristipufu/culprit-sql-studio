use super::*;
use culprit_collector::analytical::test_support::{InstanceResourceCase, instance_resource_evidence};
use culprit_core::{SourceState, telemetry::*};

#[test]
fn processor_cpu_text_uses_the_canonical_point_with_exact_totals_and_independent_native_clock() {
    use culprit_collector::analytical::test_support::{ProcessCpuCase, instance_resource_cpu_evidence};
    for case in ProcessCpuCase::ALL {
        let (source, rows) = instance_resource_cpu_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
        let cpu = report.process_cpu.as_ref().unwrap();
        let text = instance_resources(&report);
        assert!(text.contains(&cpu.title) && text.contains(&cpu.summary));
        assert!(text.contains(&cpu.limitations[0]));
        assert!(text.len() < 20_000);
        if case != ProcessCpuCase::Denied {
            assert!(text.contains("2020-01-01T12:00:00.5Z"));
        }
        if case == ProcessCpuCase::Exact {
            assert!(text.contains(&culprit_core::format::digits("9007199254740993".into())));
        }
        if case == ProcessCpuCase::Null {
            assert!(text.contains(&t!("view.database_properties.unknown")));
        }
    }
}

fn instance_report(case: InstanceResourceCase) -> InstanceResourceReport {
    let (source, rows) = instance_resource_evidence(case);
    let observations = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    let read = InstanceResourceRead::from_source(&source, &observations, true).unwrap();
    culprit_checks::instance_resources::explain(&read).unwrap()
}

#[test]
fn instance_text_keeps_exact_native_values_unknown_fields_and_shared_historical_qualification() {
    let report = instance_report(InstanceResourceCase::Exact);
    let text = instance_resources(&report);
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.database_properties.unknown")));
    let native_clock = report.observed_at.as_deref().expect("the original native observation is retained");
    let (from, until) = report.context.as_ref().expect("the original read bounds are retained").bounds();
    for original_clock in [native_clock, from, until] {
        assert!(text.contains(original_clock), "the original clock must be printed verbatim: {original_clock}");
    }
    assert!(text.contains(&report.summary));
    for group in &report.groups {
        assert!(text.contains(&group.title));
        for field in &group.fields {
            assert!(text.contains(&field.label));
        }
    }
    assert!(text.len() < 16_384);
}

#[test]
fn resource_summaries_keep_source_gaps_with_one_bounded_general_qualification() {
    for case in InstanceResourceCase::ALL {
        let report = instance_report(case);
        let text = instance_resources(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        let qualification = report.limitations.first().expect("the shared-resource scope is qualified");
        assert!(text.ends_with(qualification));
        for folded_limitation in report.limitations.iter().skip(1) {
            assert!(!text.contains(folded_limitation), "the shared brief renderer prints one general qualification");
        }
        match report.source_state {
            SourceState::Partial => assert!(text.contains(&t!("evidence.cli.source_partial"))),
            SourceState::Unavailable | SourceState::NotApplicable => {
                assert!(text.contains(&t!("evidence.cli.source_unavailable")));
            }
            _ => (),
        }
        if case == InstanceResourceCase::Denied {
            assert!(report.groups.is_empty());
            assert!(report.observed_at.is_none());
        }
        assert!(text.len() < 16_384);
    }
}

#[test]
fn scheduler_text_retains_each_native_field_exact_values_nullable_states_and_original_source_qualification() {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in SchedulerCase::ALL {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
        let schedulers = report.schedulers.as_ref().unwrap();
        let text = instance_resources(&report);
        assert!(text.contains(&schedulers.title) && text.contains(&schedulers.summary));
        assert!(text.contains(&schedulers.limitations[0]));
        for entry in schedulers.entries.iter().take(3) {
            assert!(text.contains(&entry.title));
            for field in &entry.fields {
                assert!(text.contains(&field.label));
            }
        }
        if case == SchedulerCase::NativeNull {
            assert!(text.contains(&t!("view.schedulers.value.null")));
        }
        if case == SchedulerCase::ColumnUnavailable {
            assert!(text.contains(&t!("view.schedulers.value.unavailable")));
        }
        if case == SchedulerCase::Exact {
            assert!(text.contains("9007199254740993"));
        }
        assert!(!text.contains("native-process-a") && !text.contains("native-instance-a"));
        assert!(text.len() < 32_768, "{case:?}:{}", text.len());
    }
}

#[test]
fn scheduler_text_bounds_and_escapes_each_native_field_without_hiding_later_queue_values() {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    let (source, rows) = instance_resource_scheduler_evidence(SchedulerCase::Limited);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    let mut report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
    let schedulers = report.schedulers.as_mut().unwrap();
    schedulers.entries[0].fields[0].value = Some(format!("\x1b[31m\n{}", "x".repeat(4096)));
    let extra = schedulers.entries[0].fields[0].clone();
    schedulers.entries[0].fields.extend(vec![extra; 100]);
    let text = instance_resources(&report);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains(&"x".repeat(141)));
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.schedulers.limit.presentation")));
    assert!(!text.contains(&t!("view.schedulers.entry", id = "3")));
    assert!(text.len() < 32_768);
}

#[test]
fn modern_scheduler_cli_never_words_raw_spinlock_zero_or_positive_as_measured_activity() {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in [SchedulerCase::Exact, SchedulerCase::SpinlockRawPositive] {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
        let text = instance_resources(&report);
        assert!(text.contains(&t!(
            "view.schedulers.value.unqualified",
            value = if case == SchedulerCase::Exact { "0" } else { "7" }
        )));
        assert!(text.contains(&t!("view.schedulers.counter.instrumentation", flag = "8134")));
        assert!(text.contains(&t!("view.schedulers.field.queued_disk_io_count")));
        assert!(text.contains(&t!("view.schedulers.field.total_completed_tasks")));
    }
}

#[test]
fn numa_cli_keeps_both_original_populations_exact_values_and_separate_null_or_unavailable_fields() {
    use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
    for case in NumaNodeCase::ALL {
        let (source, rows) = numa_node_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
        let nodes = report.numa_nodes.as_ref().unwrap();
        let text = instance_resources(&report);
        for population in &nodes.populations {
            assert!(text.contains(&population.title));
        }
        if case == NumaNodeCase::Exact {
            assert!(text.contains("-9223372036854775808") && text.contains("9007199254740993"));
            let (from, until) = nodes.context.as_ref().unwrap().bounds();
            assert!(text.contains(from) && text.contains(until));
        }
        if case == NumaNodeCase::NativeNull {
            assert!(text.contains(&t!("view.schedulers.value.null")));
        }
        if case == NumaNodeCase::ColumnUnavailable {
            assert!(text.contains(&t!("view.schedulers.value.unavailable")));
        }
        assert!(text.len() < 32_768, "{case:?}:{}", text.len());
        assert!(!text.contains("source_sha256") && !text.contains("native-process-a"));
    }
}
#[test]
fn numa_cli_bounds_each_field_and_entry_without_hiding_the_other_native_population() {
    use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
    let (source, rows) = numa_node_evidence(NumaNodeCase::Limited);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    let mut report = culprit_checks::instance_resources::explain_source(&source, &rows, true).unwrap();
    let nodes = report.numa_nodes.as_mut().unwrap();
    nodes.populations[0].entries[0].fields[0].value = Some(format!("\x1b[31m\n{}", "x".repeat(4096)));
    let extra = nodes.populations[0].entries[0].fields[0].clone();
    nodes.populations[0].entries[0].fields.extend(vec![extra; 100]);
    let text = instance_resources(&report);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains(&"x".repeat(141)));
    assert!(text.contains("9007199254740993"));
    assert!(!text.contains(&t!("view.numa_nodes.entry", id = "3")));
    let compact = compact_numa_nodes(&report);
    let populations = compact["populations"].as_array().unwrap();
    assert_eq!(populations.len(), 2);
    assert_eq!(populations[0]["entries"].as_array().unwrap().len(), 3);
    assert_eq!(populations[0]["entries"][0]["fields"].as_array().unwrap().len(), 13);
    assert!(populations[0]["entries"][0]["fields"][0]["value"].as_str().unwrap().chars().count() <= 141);
}
