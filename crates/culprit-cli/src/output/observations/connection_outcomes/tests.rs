use super::*;
use culprit_collector::analytical::test_support::connection_outcome_evidence_cases;
use culprit_core::{ConnectionOutcomeSnapshot, Snapshot, telemetry::*};

fn report(case: usize) -> ConnectionOutcomeReport {
    let (source, records) = connection_outcome_evidence_cases().remove(case);
    let rows = records
        .into_iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!("native counter set") };
            row
        })
        .collect();
    let read = ConnectionOutcomeSnapshot::from_source(&source, rows).unwrap();
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/postgres-demo.json")))
            .unwrap();
    snapshot.server.engine = source.target.engine();
    snapshot.server.database = "application".into();
    snapshot.bind_identity(source.target.database().unwrap().clone());
    snapshot.pressure.get_or_insert_default().connection_outcomes = Some(Box::new(read));
    culprit_engine::explain_connection_outcomes(&snapshot).unwrap().unwrap()
}

#[test]
fn human_output_uses_exact_shared_native_formatting_and_explicit_unknown_coverage_and_print_limits() {
    let source_report = report(6);
    assert!(source_report.entries.len() > ENTRY_LIMIT);
    let rendered = connection_outcomes(&source_report);
    assert!(rendered.contains("9007199254740993"));
    assert!(rendered.contains("18446744073709551615"));
    assert!(!rendered.contains("9007199254740992"));
    assert!(connection_outcomes(&report(0)).contains(&t!("view.connection_outcomes.cli.missing", state = "unknown")));
    assert_eq!(rendered.matches(&source_report.entries[0].title).count(), ENTRY_LIMIT);
    assert!(rendered.contains(&t!("view.connection_outcomes.limit.presentation")));
    assert!(rendered.contains(&source_report.entries[0].reference.observation_key));
    assert!(!rendered.contains("fixture.invalid"));
}

#[test]
fn legacy_absence_partial_and_unavailable_sources_never_render_as_measured_zero_or_clean_health() {
    let legacy = connection_outcomes(&report(2));
    assert!(legacy.contains(&t!("view.connection_outcomes.cli.missing", state = "unsupported")));
    for case in [4, 5, 7, 8, 9, 10] {
        let report = report(case);
        let rendered = connection_outcomes(&report);
        for limitation in &report.limitations {
            assert!(rendered.contains(limitation));
        }
        assert!(rendered.contains(&report.title));
    }
}
