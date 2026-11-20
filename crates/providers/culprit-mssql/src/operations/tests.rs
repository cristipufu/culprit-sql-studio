use super::*;
use crate::{
    operations::native::test_support::{file_row, space_row, temp_space_row},
    resources::test_support::FakeRead,
    value::Rec,
};
use culprit_core::{SourceState, telemetry::*};
fn fresh_snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = "sample".into();
    snapshot.operations = Default::default();
    snapshot.sources.clear();
    snapshot
}
fn target() -> CollectionTarget {
    culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "sample", "fixture.invalid", 1433, None).into()
}
fn replies() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![space_row()]),
        Ok(vec![space_row(), file_row()]),
        Ok(vec![temp_space_row()]),
        Ok(vec![]),
        Ok(vec![native::suspect_pages::tests::row(0, 0)]),
    ]
}
#[tokio::test]
async fn scan_and_operational_stream_reuse_the_same_exact_points_only_after_confirmed_close() {
    let mut io = FakeRead::new(replies());
    let mut snapshot = fresh_snapshot();
    let mut probe = crate::probe::tests::probe(3, false);
    probe.database = "sample".into();
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 5);
    assert_eq!(io.closed, 1);
    assert_eq!(snapshot.operations.spaces.len(), 3);
    let read = snapshot.operations.storage.as_ref().unwrap();
    assert!(read.is_valid());
    assert_eq!(read.observations.len(), 4);
    assert!(read.context.native().is_none());
    assert!(
        snapshot.operations.spaces.iter().all(|space| space
            .origin
            .as_ref()
            .and_then(|reference| read.space(reference))
            .is_some())
    );
}
#[tokio::test]
async fn uncertain_shutdown_discards_staged_positives_and_preserves_explicit_unavailable_sources() {
    let mut io = FakeRead::new(replies());
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut snapshot = fresh_snapshot();
    let probe = crate::probe::tests::probe(3, false);
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.closed, 1);
    assert!(snapshot.operations.spaces.is_empty());
    let history = snapshot.operations.suspect_pages.as_ref().unwrap();
    assert!(history.observation.is_none());
    assert!(!history.read_complete);
    let read = snapshot.operations.storage.as_ref().unwrap();
    assert!(read.observations.is_empty());
    assert!(!read.read_complete);
    assert!(read.sources.iter().all(|source| source.state == SourceState::Unavailable
        && source.stopped == Some(CollectionStop::CancellationUnconfirmed)));
}
#[tokio::test]
async fn terminal_source_failure_stops_siblings_while_denied_metadata_keeps_independent_exact_points() {
    let mut io = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(CollectionStop::TimedOut))]);
    let mut snapshot = fresh_snapshot();
    let probe = crate::probe::tests::probe(3, false);
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 1);
    assert_eq!(io.closed, 1);
    assert!(
        snapshot
            .operations
            .storage
            .as_ref()
            .unwrap()
            .sources
            .iter()
            .all(|source| source.stopped == Some(CollectionStop::TimedOut))
    );
    let mut responses = replies();
    responses[1] = Err(crate::read::ReadError::Source(culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        "denied",
    )));
    let mut io = FakeRead::new(responses);
    let mut snapshot = fresh_snapshot();
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 5);
    assert_eq!(io.closed, 1);
    let read = snapshot.operations.storage.unwrap();
    assert_eq!(read.observations.len(), 2);
    assert_eq!(
        read.sources.iter().find(|source| source.source == culprit_core::source_id::FILE_CAPACITY).unwrap().state,
        SourceState::Unavailable
    );
}

#[tokio::test]
async fn retained_history_statuses_stay_visible_without_becoming_cumulative_checksum_failures() {
    for (active, total) in [(0, 3), (2, 4)] {
        let mut rows = replies();
        rows[4] = Ok(vec![native::suspect_pages::tests::row(active, total)]);
        let mut io = FakeRead::new(rows);
        let mut snapshot = fresh_snapshot();
        let probe = crate::probe::tests::probe(3, false);
        collect_with(&mut io, &probe, target(), &mut snapshot).await;
        let point = snapshot.operations.suspect_pages.as_ref().unwrap().observation.as_ref().unwrap();
        assert_eq!(point.retained_rows, total.to_string());
        assert_eq!(point.count, active.to_string());
        assert!(snapshot.operations.counters.is_empty());
        assert_eq!(io.calls.len(), 5);
        assert_eq!(io.closed, 1);
    }
}

async fn scan_retained_history(active: i64, retained: i64, at: &str) -> Snapshot {
    let mut responses = replies();
    responses[4] = Ok(vec![native::suspect_pages::tests::row(active, retained)]);
    let mut io = FakeRead::new(responses);
    let mut snapshot = fresh_snapshot();
    snapshot.identity = target().database().cloned();
    snapshot.taken_at = at.into();
    let probe = crate::probe::tests::probe(3, false);
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 5);
    assert_eq!(io.closed, 1);
    snapshot
}

#[tokio::test]
async fn retained_history_edits_deletions_and_repaired_rows_cannot_become_new_events_between_scans() {
    let check = culprit_checks::all().into_iter().find(|check| check.info().id == "operational-errors").unwrap();
    // A status edit can grow the active subset without adding a row; deletion can shrink either population.
    for ((old_active, old_retained), (active, retained)) in
        [((1, 3), (3, 3)), ((3, 3), (1, 3)), ((1, 3), (20, 22)), ((0, 5), (0, 1)), ((2, 4), (2, 4))]
    {
        let previous = scan_retained_history(old_active, old_retained, "2026-11-20T06:00:00Z").await;
        let mut current = scan_retained_history(active, retained, "2026-11-20T07:00:00Z").await;
        assert!(previous.same_database(&current));
        let original = serde_json::to_value(&current.operations.suspect_pages).unwrap();
        culprit_core::operations::compare(&previous, &mut current);
        assert_eq!(serde_json::to_value(&current.operations.suspect_pages).unwrap(), original);
        assert!(previous.operations.counters.is_empty());
        assert!(current.operations.counters.is_empty());
        assert!(check.run(&previous).is_empty());
        assert!(check.run(&current).is_empty());
        let report = culprit_checks::suspect_pages::explain_snapshot(&current).unwrap().unwrap();
        assert_eq!(report.active_rows.as_deref(), Some(active.to_string().as_str()));
        assert_eq!(report.retained_rows.as_deref(), Some(retained.to_string().as_str()));
        assert_eq!(report.native_database_id.as_deref(), Some("7"));
        assert_eq!(report.source_state, Some(SourceState::Collected));
        assert_eq!(report.metrics.len(), 2);
        for limitation in [
            culprit_core::t!("view.suspect_pages.limit.editable"),
            culprit_core::t!("view.suspect_pages.limit.incarnation"),
            culprit_core::t!("view.suspect_pages.limit.zero"),
        ] {
            assert!(report.limitations.contains(&limitation));
        }
        assert_eq!(
            report.summary,
            culprit_core::t!(
                "view.suspect_pages.recorded",
                retained = culprit_core::format::digits(retained.to_string()),
                active = culprit_core::format::digits(active.to_string())
            )
        );
        let read = current.operations.suspect_pages.as_ref().unwrap();
        let acquisition = read.context.native().expect("the original acquisition retains its native clock");
        let point = read.observation.as_ref().unwrap();
        assert_eq!(acquisition.target, read.target);
        assert_eq!(acquisition.observed_from, point.observed_at);
        assert_eq!(acquisition.observed_until, point.observed_at);
        // Native acquisition context supplies no historical ownership or reset proof for these editable rows.
        assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
    }
}

#[tokio::test]
async fn retained_history_unknown_identity_never_establishes_an_event_interval() {
    let previous = scan_retained_history(1, 3, "2026-11-20T06:00:00Z").await;
    let mut current = scan_retained_history(20, 22, "2026-11-20T07:00:00Z").await;
    current.identity = None;
    assert!(!previous.same_database(&current));
    culprit_core::operations::compare(&previous, &mut current);
    assert!(current.operations.counters.is_empty());
    let report = culprit_checks::suspect_pages::explain_snapshot(&current).unwrap().unwrap();
    assert_eq!(report.active_rows.as_deref(), Some("20"));
    assert_eq!(report.retained_rows.as_deref(), Some("22"));
    assert!(report.limitations.contains(&culprit_core::t!("view.suspect_pages.limit.incarnation")));
    assert!(report.limitations.contains(&culprit_core::t!("view.suspect_pages.limit.zero")));
}
