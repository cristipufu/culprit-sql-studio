use super::*;
use culprit_core::{Engine, SourceState};

fn snapshot() -> Snapshot {
    let read = crate::connection_outcomes::tests::snapshot(180000);
    let mut snapshot = crate::waits::snapshot_with(Engine::Postgres);
    snapshot.server.database = "fixture".into();
    snapshot.bind_identity(read.target.database().unwrap().clone());
    snapshot.pressure.get_or_insert_default().connection_outcomes = Some(Box::new(read));
    snapshot
}

#[test]
fn unknown_outer_identity_preserves_an_unverified_independent_view_of_exact_native_positives() {
    let mut snapshot = snapshot();
    snapshot.identity = None;
    let report = explain(&mut snapshot).unwrap();
    assert_eq!((report.native_rows, report.observed_rows), (1, 1));
    assert_eq!(report.sources[0].state, SourceState::Collected);
    assert_eq!(
        report.entries[0].metrics[0].value,
        Some(culprit_core::telemetry::MeasurementValue::Integer("9007199254740993".into()))
    );
    assert!(report.limitations.contains(&t!("view.observations.limit.namespace")));
    assert!(
        !snapshot
            .pressure
            .as_ref()
            .unwrap()
            .connection_outcomes
            .as_ref()
            .unwrap()
            .target
            .snapshot_namespace_comparison(&snapshot)
            .allows_comparison()
    );
}

#[test]
fn conflicting_database_engine_or_known_incarnation_cannot_be_explained_as_this_scan() {
    for change in ["database", "engine", "incarnation"] {
        let mut snapshot = snapshot();
        match change {
            "database" => snapshot.server.database = "another".into(),
            "engine" => snapshot.server.engine = Engine::Mysql,
            "incarnation" => {
                let read = snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap();
                let culprit_core::telemetry::CollectionTarget::Database(database) = &mut read.target else {
                    unreachable!()
                };
                database.incarnation = Some("captured-db".into());
                snapshot.identity.as_mut().unwrap().incarnation = Some("other-native-db".into());
            }
            _ => unreachable!(),
        }
        assert!(explain(&mut snapshot).is_none(), "{change}");
        assert_eq!(
            snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap().observations.len(),
            1,
            "raw input retained"
        );
    }
}
