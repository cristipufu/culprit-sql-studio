use super::*;
use culprit_checks::memory_clerks::MemoryClerkEvidence;
use culprit_collector::analytical::test_support::{MemoryClerkCase, memory_clerk_evidence};
use culprit_core::telemetry::*;

fn report(case: MemoryClerkCase, missing: bool) -> MemoryClerkReport {
    let (source, records) = memory_clerk_evidence(case);
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let rows = records
        .iter()
        .filter_map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            (!missing && row.source == culprit_core::source_id::MEMORY_CLERKS).then_some(row)
        })
        .collect::<Vec<_>>();
    culprit_checks::memory_clerks::explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage,
        observations: &rows,
        read_complete: !missing,
    })
    .unwrap()
}

#[test]
fn human_clerk_output_keeps_exact_units_nodes_native_bounds_and_every_finite_qualification() {
    let report = report(MemoryClerkCase::Exact, false);
    let text = memory_clerks(&report);
    assert!(text.contains("9007199254740993"));
    assert!(!text.contains("9007199254740992"));
    assert!(text.contains("2020-01-01T12:00:00.000000001Z"));
    assert!(text.contains("2020-01-01T12:00:00.000000009Z"));
    for entry in &report.entries {
        assert!(text.contains(&entry.clerk_type));
        assert!(text.contains(&entry.name));
        for field in &entry.fields {
            assert!(text.contains(&field.label));
        }
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains(&t!("collected.memory_clerks.unknown")));
}

#[test]
fn missing_bodies_and_denied_sources_show_original_counts_and_do_not_print_allocated_zero() {
    for report in [report(MemoryClerkCase::Exact, true), report(MemoryClerkCase::Denied, false)] {
        assert!(report.entries.is_empty());
        let text = memory_clerks(&report);
        assert!(text.contains(&report.summary));
        assert!(!text.contains("9007199254740993"));
        assert!(!text.contains(&t!("collected.memory_clerks.field.pages_kb")));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
    }
    let unknown = report(MemoryClerkCase::Unreported, false);
    let text = memory_clerks(&unknown);
    assert!(text.contains(&t!("collected.memory_clerks.unknown")));
    assert!(unknown.entries.iter().all(|entry| {
        entry.fields.iter().filter(|field| field.key != "clerk_count").all(|field| field.value.is_none())
    }));
}

#[test]
fn printed_detail_cap_never_changes_original_observed_or_backend_presentation_counts() {
    let (mut source, records) = memory_clerk_evidence(MemoryClerkCase::Exact);
    let TelemetryRecord::Operational(template) = &records[0] else { panic!() };
    let observations = (0..300)
        .map(|node| {
            let mut row = template.clone();
            let OperationalFact::CounterSet(set) = &mut row.fact else { panic!() };
            let name = format!("visible-clerk-{node}");
            set.properties.insert("name".into(), name.clone());
            set.properties.insert("memory_node_id".into(), node.to_string());
            set.identity.qualifiers.insert("name_sha256".into(), digest(name.as_bytes()));
            set.identity.qualifiers.insert("memory_node_id".into(), node.to_string());
            row.key = set.key().unwrap();
            row
        })
        .collect::<Vec<_>>();
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.records = 300;
    snapshot.sources = vec![StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 300)];
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let rows = observations.iter().collect::<Vec<_>>();
    let report = culprit_checks::memory_clerks::explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage: &source.observation.snapshot().unwrap().sources[0],
        observations: &rows,
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.native_rows, Some(300));
    assert_eq!(report.observed_rows, 300);
    assert_eq!(report.presented_rows, 256);
    let printed = super::super::super::BRIEF_FINDING_LIMIT;
    let text = memory_clerks(&report);
    assert!(text.contains(&format!("visible-clerk-{}", printed - 1)));
    assert!(!text.contains(&format!("visible-clerk-{printed}")));
    assert!(text.contains(&t!(
        "view.wait_counters.cli.counts",
        native = "300",
        observed = "300",
        presented = "256",
        printed = printed.to_string()
    )));
    assert!(text.contains(&t!("collected.memory_clerks.limit.presentation")));
}
