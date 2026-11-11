use super::*;
use crate::telemetry::activity::tests::request;

fn source(at: &str, qualifying: u32) -> CollectionDescription {
    CollectionDescription {
        database: crate::DatabaseIdentity::server(crate::Engine::Postgres, "fixture", "fixture.invalid", 5432, None),
        source: "activity-fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: "native-epoch-1".into(),
            at: at.into(),
            database_wide: true,
            policy: ActivityPolicy::default(),
            native_rows: qualifying,
            qualifying,
            observations: qualifying,
            blocking_edges: 0,
            details: ActivityDetailStatus::NotNeeded,
        }),
    }
}
fn capture(id: &str) -> CaptureIdentity {
    CaptureIdentity { observation_id: id.into(), revision: None }
}
#[test]
fn incident_start_change_suppression_periodic_update_and_resolution_are_distinct() {
    let first =
        ActivityIncidentUpdate::observe(&source("2020-01-01T00:00:00Z", 1), &capture("1"), &[request("1")], &[])
            .unwrap();
    assert_eq!(first.changes[0].transition, ActivityIncidentTransition::Started);
    let second = ActivityIncidentUpdate::observe(
        &source("2020-01-01T00:01:00Z", 1),
        &capture("2"),
        &[request("1")],
        &first.active,
    )
    .unwrap();
    assert_eq!(second.active[0].id, first.active[0].id);
    assert_eq!(second.active[0].progress, ActivityProgress::NoObservedCounterChange);
    let third = ActivityIncidentUpdate::observe(
        &source("2020-01-01T00:02:00Z", 1),
        &capture("3"),
        &[request("1")],
        &second.active,
    )
    .unwrap();
    assert!(third.changes.is_empty());
    let periodic = ActivityIncidentUpdate::observe(
        &source("2020-01-01T00:06:00Z", 1),
        &capture("4"),
        &[request("1")],
        &third.active,
    )
    .unwrap();
    assert_eq!(periodic.changes.len(), 1);
    let resolved =
        ActivityIncidentUpdate::observe(&source("2020-01-01T00:07:00Z", 0), &capture("5"), &[], &periodic.active)
            .unwrap();
    assert!(resolved.active.is_empty());
    assert_eq!(resolved.changes[0].transition, ActivityIncidentTransition::Resolved);
    assert_eq!(resolved.changes[0].state.last_seen_at, "2020-01-01T00:06:00Z");
    assert_eq!(resolved.changes[0].at, "2020-01-01T00:07:00Z");
}
#[test]
fn a_new_request_generation_cannot_inherit_an_old_incident() {
    let before =
        ActivityIncidentUpdate::observe(&source("2020-01-01T00:00:00Z", 1), &capture("1"), &[request("1")], &[])
            .unwrap();
    let mut reused = request("1");
    reused.key.request.as_mut().unwrap().generation = "request-2".into();
    let now =
        ActivityIncidentUpdate::observe(&source("2020-01-01T00:01:00Z", 1), &capture("2"), &[reused], &before.active)
            .unwrap();
    assert_ne!(now.active[0].id, before.active[0].id);
    assert_eq!(now.changes.len(), 2);
}
#[test]
fn restricted_visibility_cannot_resolve_existing_incidents() {
    let before =
        ActivityIncidentUpdate::observe(&source("2020-01-01T00:00:00Z", 1), &capture("1"), &[request("1")], &[])
            .unwrap();
    let mut restricted = source("2020-01-01T00:01:00Z", 0);
    let SourceObservation::Activity(snapshot) = &mut restricted.observation else { unreachable!() };
    snapshot.database_wide = false;
    assert!(ActivityIncidentUpdate::observe(&restricted, &capture("2"), &[], &before.active).is_err());
    assert_eq!(before.active.len(), 1);
}
#[test]
fn the_same_native_activity_in_another_epoch_has_an_independent_identity() {
    let original = source("2020-01-01T00:00:00Z", 1);
    let mut replacement = original.clone();
    let SourceObservation::Activity(snapshot) = &mut replacement.observation else { unreachable!() };
    snapshot.epoch = "native-epoch-2".into();
    assert_ne!(original.scoped_id("scope", &()).unwrap(), replacement.scoped_id("scope", &()).unwrap());
}
