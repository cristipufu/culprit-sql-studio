use super::*;
use culprit_core::{ComparisonReason, DatabaseIdentity, Engine, SourceState};

fn snapshot() -> Box<culprit_core::Snapshot> {
    let mut snapshot: Box<culprit_core::Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/postgres-demo.json")))
            .unwrap();
    let read = input();
    let database = read.context.target.database().unwrap().clone();
    snapshot.server.engine = database.engine;
    snapshot.server.database = database.database.clone();
    snapshot.bind_identity(database);
    snapshot.operations.jobs =
        Some(Box::new(JobsSnapshot::Timescale { snapshot: Box::new(read), read_complete: true }));
    snapshot
}
fn replace_target(snapshot: &mut culprit_core::Snapshot, change: &str) {
    let JobsSnapshot::Timescale { snapshot: read, .. } = snapshot.operations.jobs.as_mut().unwrap().as_mut() else {
        panic!()
    };
    let target = &mut read.context.target;
    let CollectionTarget::Database(database) = target else { panic!() };
    match change {
        "database" => database.database = "another-database".into(),
        "endpoint" => {
            *database =
                DatabaseIdentity::server(Engine::Postgres, &database.database, "another-host.invalid", 5432, None)
        }
        "port" => {
            *database = DatabaseIdentity::server(Engine::Postgres, &database.database, "private-host", 55432, None)
        }
        "incarnation" => {
            database.incarnation = Some("captured-native-db".into());
            snapshot.identity.as_mut().unwrap().incarnation = Some("different-native-db".into());
        }
        _ => panic!("unknown controlled namespace change"),
    }
}
#[test]
fn matching_namespace_reuses_the_raw_view_without_modification_or_inferred_attribution() {
    let snapshot = snapshot();
    let raw = snapshot.operations.jobs.as_ref().unwrap();
    let expected = explain(raw).unwrap();
    let before = serde_json::to_value(raw).unwrap();
    let actual = explain_snapshot(&snapshot).unwrap().unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(serde_json::to_value(raw).unwrap(), before);
}
#[test]
fn valid_foreign_job_namespaces_are_rejected_without_invalidating_independent_archive_evidence() {
    for change in ["database", "endpoint", "port", "incarnation", "outer-engine"] {
        let mut snapshot = snapshot();
        if change == "outer-engine" {
            snapshot.server.engine = Engine::Mysql;
        } else {
            replace_target(&mut snapshot, change);
        }
        let read = snapshot.operations.jobs.as_ref().unwrap();
        assert!(read.is_valid(), "{change}: the original source remains individually valid");
        let before = serde_json::to_value(read).unwrap();
        assert!(explain_snapshot(&snapshot).is_err(), "{change}");
        let independent = explain(read).unwrap();
        assert_eq!((independent.native_rows, independent.observed_rows), (1, 1));
        assert!(independent.entries[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
        assert_eq!(serde_json::to_value(read).unwrap(), before);
    }
}
#[test]
fn unknown_outer_identity_keeps_exact_independent_positives_with_explicit_namespace_limits() {
    for unknown in ["missing", "unknown-location", "one-unknown-incarnation"] {
        let mut snapshot = snapshot();
        match unknown {
            "missing" => snapshot.identity = None,
            "unknown-location" => {
                snapshot.identity.as_mut().unwrap().location = culprit_core::DatabaseLocation::Unknown
            }
            _ => snapshot.identity.as_mut().unwrap().incarnation = Some("known-outer-db".into()),
        }
        let read = snapshot.operations.jobs.as_ref().unwrap();
        assert_eq!(
            read.context().target.snapshot_namespace_comparison(&snapshot).reason(),
            Some(&ComparisonReason::UnknownIdentity)
        );
        let actual = explain_snapshot(&snapshot).unwrap().unwrap();
        assert_eq!(actual.context, *read.context(), "the enclosing scan cannot replace original ownership");
        assert!(actual.limitations.contains(&t!("view.observations.limit.namespace")));
        assert_eq!((actual.native_rows, actual.observed_rows), (1, 1));
    }
}
#[test]
fn the_scan_pipeline_retains_conflicting_raw_facts_but_emits_an_unavailable_source_instead_of_a_job_report() {
    let mut snapshot = snapshot();
    replace_target(&mut snapshot, "database");
    let original = serde_json::to_value(&snapshot.operations.jobs).unwrap();
    let report = crate::analyze(*snapshot);
    assert!(report.jobs.is_none());
    assert_eq!(serde_json::to_value(&report.snapshot.operations.jobs).unwrap(), original);
    assert!(report.snapshot.sources.iter().any(|source| source.source == TIMESCALE_JOBS_SOURCE
        && source.state == SourceState::Unavailable
        && source.reason_message == Some(culprit_core::message!("view.jobs.invalid"))));
}
