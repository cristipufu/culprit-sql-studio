use super::*;
use crate::{
    oom_events::tests::{platform, replace, row},
    resources::{pool::test_support::identity_row, test_support::FakeRead},
    value::{Rec, Val},
};
use culprit_core::{Error, SourceState};

fn state(coverage: &Coverage) -> SourceState {
    coverage.outcomes().into_iter().find(|outcome| outcome.source == sources::MEMORY_FAILURES.name).unwrap().state
}

#[test]
fn the_native_profile_preserves_the_scan_ceiling_and_stricter_caller_limits() {
    let scan = budget();
    assert_eq!(scan.max_queries, 6);
    assert_eq!(scan.max_snapshot_rows, 128);
    assert_eq!(scan.page_bytes, 8 * 1024 * 1024);
    assert_eq!(scan.statement_timeout_ms, 3000);
    assert_eq!(scan.total_timeout_ms, 10_000);
    let expanded = CollectionBudget {
        max_snapshot_rows: 1000,
        page_rows: 1000,
        max_queries: 1000,
        page_bytes: 8 * 1024 * 1024,
        total_bytes: 1024 * 1024 * 1024,
        statement_timeout_ms: 60_000,
        total_timeout_ms: 120_000,
        ..Default::default()
    };
    assert_eq!(bounded_budget(&expanded, 0), scan);
    let strict = CollectionBudget {
        max_snapshot_rows: 1,
        page_rows: 1,
        max_queries: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        statement_timeout_ms: 1,
        total_timeout_ms: 2,
    };
    strict.validate().unwrap();
    assert_eq!(bounded_budget(&strict, 0), strict);
    assert_eq!(bounded_budget(&strict, 2), strict);
}
fn pool_reader(after: Rec) -> FakeRead {
    FakeRead::new(vec![Ok(vec![identity_row()]), Ok(vec![platform(5)]), Ok(vec![row(), row()]), Ok(vec![after])])
}
fn mi_metadata() -> Rec {
    replace(crate::instance::test_support::metadata(), "edition", Val::Int(8))
}

#[tokio::test]
async fn opt_out_and_boxed_products_add_no_reads_or_false_empty_measurements() {
    for (requested, edition) in [(false, 5), (false, 8), (true, 2), (true, 3), (true, 4)] {
        let mut reader = FakeRead::new(vec![]);
        let (snapshot, coverage) = collect_from(&mut reader, requested, edition).await;
        assert!(snapshot.is_none());
        assert_eq!(state(&coverage), SourceState::NotApplicable);
        assert!(reader.calls.is_empty());
        assert_eq!(reader.closed, 0);
    }
    let mut reader = FakeRead::new(vec![]);
    let (snapshot, coverage) = collect_from(&mut reader, true, 99).await;
    assert!(snapshot.is_none());
    assert_eq!(state(&coverage), SourceState::Unavailable);
    assert!(reader.calls.is_empty());
}

#[tokio::test]
async fn a_verified_pool_snapshot_retains_duplicates_scope_and_no_invented_query_owner() {
    let mut reader = pool_reader(identity_row());
    let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
    let snapshot = snapshot.unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.scope, MeasurementScope::Pool);
    assert_eq!(snapshot.resource.unwrap().kind, SharedResourceKind::Pool);
    assert_eq!(snapshot.observations.iter().map(|row| row.position).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(state(&coverage), SourceState::Collected);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(reader.closed, 1);
    let budget = budget();
    budget.validate().unwrap();
    assert_eq!(budget.max_queries, 6);
    assert_eq!(reader.calls[2].limit, MAX_ROWS + 1);
    assert!(reader.calls.iter().all(|call| !call.sql.contains("dm_xe") && !call.sql.contains("sys.dm_exec_requests")));
}

#[tokio::test]
async fn unpooled_context_remains_unowned_but_its_physical_route_is_still_verified() {
    let before = replace(identity_row(), "pool_name", Val::Null);
    let mut reader = FakeRead::new(vec![Ok(vec![before.clone()]), Ok(vec![platform(5)]), Ok(vec![]), Ok(vec![before])]);
    let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot.scope, MeasurementScope::Server);
    assert_eq!(snapshot.resource, None);
    assert!(snapshot.observations.is_empty());
    assert_eq!(state(&coverage), SourceState::Empty);
    let before = replace(identity_row(), "pool_name", Val::Null);
    let after = replace(before.clone(), "physical_incarnation", Val::Text("new-physical-member".into()));
    let mut reader = FakeRead::new(vec![Ok(vec![before]), Ok(vec![platform(5)]), Ok(vec![row()]), Ok(vec![after])]);
    let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
    assert!(snapshot.is_none());
    assert_eq!(state(&coverage), SourceState::Unavailable);
}

#[tokio::test]
async fn managed_instance_uses_its_instance_identity_and_epoch_with_the_same_native_reader() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![mi_metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![platform(8)]),
        Ok(vec![row()]),
        Ok(vec![mi_metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
    ]);
    let (snapshot, coverage) = collect_from(&mut reader, true, 8).await;
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot.scope, MeasurementScope::Server);
    assert_eq!(snapshot.resource.unwrap().kind, SharedResourceKind::Instance);
    assert_eq!(state(&coverage), SourceState::Collected);
    assert_eq!(reader.calls.len(), MAX_QUERIES as usize);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn a_changed_pool_role_or_uncertain_close_discards_staged_observations() {
    for after in [
        replace(identity_row(), "pool_name", Val::Text("other-pool".into())),
        replace(identity_row(), "physical_incarnation", Val::Text("other-process".into())),
        replace(identity_row(), "updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut reader = pool_reader(after);
        let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
        assert!(snapshot.is_none());
        assert_eq!(state(&coverage), SourceState::Unavailable);
        assert_eq!(reader.closed, 1);
    }
    let mut reader = pool_reader(identity_row());
    reader.shutdown = SourceShutdown::Unconfirmed;
    let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
    assert!(snapshot.is_none());
    assert_eq!(state(&coverage), SourceState::Unavailable);
}

#[tokio::test]
async fn native_failure_stops_rechecks_and_only_confirmed_permissions_add_source_guidance() {
    for error in [
        CollectionStop::TimedOut.error(),
        CollectionStop::CancellationUnconfirmed.error(),
        Error::new(ErrorCode::PermissionDenied, "native OOM view denied"),
    ] {
        let permission = error.code == ErrorCode::PermissionDenied;
        let mut reader = FakeRead::new(vec![Ok(vec![identity_row()]), Ok(vec![platform(5)]), Err(error.into())]);
        let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
        assert!(snapshot.is_none());
        assert_eq!(reader.calls.len(), 3, "no source replacement or final identity query after native failure");
        assert_eq!(reader.closed, 1);
        assert_eq!(state(&coverage), SourceState::Unavailable);
        let (_, _, notes) = coverage.finish();
        assert_eq!(notes.iter().any(|note| note.message.contains("VIEW SERVER PERFORMANCE STATE")), permission);
    }
    let mut reader = FakeRead::new(vec![Err(Error::new(ErrorCode::PermissionDenied, "pool metadata denied").into())]);
    let (snapshot, coverage) = collect_from(&mut reader, true, 5).await;
    assert!(snapshot.is_none());
    assert_eq!(reader.calls.len(), 1);
    let (_, _, notes) = coverage.finish();
    assert!(
        !notes.iter().any(|note| note.message.contains("VIEW SERVER PERFORMANCE STATE")),
        "metadata denial cannot invent an OOM-view grant requirement"
    );
}
