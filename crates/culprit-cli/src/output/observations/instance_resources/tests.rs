use super::*;
use culprit_collector::analytical::test_support::{InstanceResourceCase, instance_resource_evidence};
use culprit_core::{SourceState, telemetry::*};

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
