use crate::completed_spills::tests::fixture;
use culprit_core::{Engine, Snapshot, SourceState, telemetry::*};

fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut read = fixture::Evidence::runtime(Some("9007199254740993")).runtime_read();
    read.description.target = snapshot.database_identity().into();
    snapshot.operations.cached_runtime = Some(Box::new(read));
    snapshot
}

#[test]
fn pipeline_explains_exact_entries_without_replacing_or_lending_the_aggregate_population() {
    let original = snapshot();
    let aggregate = serde_json::to_value(&original.workload).unwrap();
    let expected = crate::completed_spills::explain_snapshot(&original).unwrap().unwrap();
    let report = crate::analyze(original);
    let explained = report.completed_spills.as_ref().unwrap();
    assert_eq!(explained.entries[0].identity, expected.entries[0].identity);
    assert_eq!(
        serde_json::to_value(&explained.entries[0].metrics).unwrap(),
        serde_json::to_value(&expected.entries[0].metrics).unwrap()
    );
    assert_eq!(serde_json::to_value(&report.evidence.as_ref().unwrap().input.workload).unwrap(), aggregate);
    assert!(report.snapshot.operations.cached_runtime.is_some());
}

#[test]
fn ordinary_aggregate_snapshots_do_not_manufacture_entry_evidence() {
    let mut snapshot = snapshot();
    snapshot.operations.cached_runtime = None;
    assert!(crate::completed_spills::explain_snapshot(&snapshot).unwrap().is_none());
    assert!(crate::analyze(snapshot).completed_spills.is_none());
}

#[test]
fn known_foreign_namespaces_are_withheld_while_unknown_scan_identity_remains_explicit() {
    for change in ["engine", "database", "endpoint", "incarnation"] {
        let mut snapshot = snapshot();
        let read = snapshot.operations.cached_runtime.as_mut().unwrap();
        let CollectionTarget::Database(database) = &mut read.description.target else { panic!() };
        match change {
            "engine" => database.engine = Engine::Postgres,
            "database" => database.database = "another-database".into(),
            "endpoint" => {
                database.incarnation = None;
                database.location = culprit_core::DatabaseLocation::Server {
                    host: "foreign-cache.invalid".into(),
                    port: 1433,
                    instance: None,
                };
                snapshot.identity.as_mut().unwrap().incarnation = None;
            }
            "incarnation" => {
                database.incarnation = Some("another-native-incarnation".into());
                snapshot.identity.as_mut().unwrap().incarnation = Some("original-native-incarnation".into());
            }
            _ => unreachable!(),
        }
        assert!(crate::completed_spills::explain_snapshot(&snapshot).is_err(), "{change}");
        assert!(super::explain(&mut snapshot).is_none());
        assert!(
            snapshot
                .sources
                .iter()
                .any(|source| source.source == CACHED_RUNTIME_READ_SOURCE && source.state == SourceState::Unavailable)
        );
    }
    let mut snapshot = snapshot();
    snapshot.identity = None;
    let explained = crate::completed_spills::explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(explained.limitations.contains(&culprit_core::t!("view.observations.limit.namespace")));
    assert_eq!(explained.entries.len(), 1);
}

#[test]
fn a_partial_body_preserves_original_native_scope_and_never_disappears_into_an_empty_healthy_report() {
    let mut snapshot = snapshot();
    let read = snapshot.operations.cached_runtime.as_mut().unwrap();
    read.observations.clear();
    read.read_complete = false;
    let report = super::explain(&mut snapshot).unwrap();
    assert_eq!((report.native_rows, report.observed_rows), (1, 0));
    assert!(!report.read_complete);
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert!(report.entries.is_empty());
    assert!(
        snapshot.operations.cached_runtime.as_ref().unwrap().description.observation.snapshot().unwrap().sources[0]
            .complete_scope
    );
}
