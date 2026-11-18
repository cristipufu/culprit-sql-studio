use super::*;
use crate::{
    activity::memory::tests::{closed, columns, context, reader, reader_with_options, row, set_shutdown, successful},
    resources::test_support::replace,
    value::Val,
};
use culprit_core::SourceState;

fn state(coverage: &Coverage) -> SourceState {
    coverage.outcomes().into_iter().find(|source| source.source == sources::CURRENT_REQUEST_MEMORY.name).unwrap().state
}

#[test]
fn native_profile_caps_default_and_expanded_requests_without_widening_stricter_callers() {
    for requested in [
        CollectionBudget::default(),
        CollectionBudget {
            max_snapshot_rows: 10_000,
            page_rows: 1000,
            max_queries: 1000,
            page_bytes: 8 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            statement_timeout_ms: 60_000,
            total_timeout_ms: 120_000,
            ..Default::default()
        },
    ] {
        let actual = bounded_budget(&requested);
        actual.validate().unwrap();
        assert_eq!(actual.max_snapshot_rows, 200);
        assert_eq!(actual.page_rows, 200);
        assert_eq!(actual.max_queries, 9);
        assert_eq!(actual.page_bytes, 4 * 1024 * 1024);
        assert_eq!(actual.total_bytes, 64 * 1024 * 1024);
        assert_eq!(actual.max_sql_bytes, 16 * 1024);
        assert_eq!(actual.statement_timeout_ms, 3000);
        assert_eq!(actual.total_timeout_ms, 10_000);
    }
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
    assert_eq!(bounded_budget(&strict), strict);
}

#[tokio::test]
async fn default_scan_policy_does_not_open_a_session_or_read_current_grants() {
    let pool = Arc::new(Pool::new(crate::test_support::config(1, "not-a-database", "not-a-password", "culprit"), None));
    let (rows, coverage) = collect(pool, false, "no-native-request.invalid".into(), 1).await;
    assert!(rows.is_none());
    assert_eq!(state(&coverage), SourceState::NotApplicable);
}

#[tokio::test]
async fn an_explicit_scan_stages_grants_through_identity_verification_and_confirmed_shutdown() {
    let mut reader = successful();
    let (rows, coverage) = collect_from(&mut reader).await;
    assert_eq!(rows.unwrap().observations.len(), 1);
    assert_eq!(state(&coverage), SourceState::Collected);
    assert_eq!(reader.query_count(), 6, "Azure source ownership has two initial and one final metadata phases");
    assert_eq!(closed(&reader), 1);
}

#[tokio::test]
async fn unassigned_grants_remain_visible_as_partial_evidence_without_a_database_measurement() {
    let unassigned = replace(&row(), &[("request_generation", Val::Text("2020-01-01T07:59:00".into()))]);
    let mut reader = reader(vec![
        Ok(vec![crate::grants::tests::platform()]),
        Ok(vec![context()]),
        Ok(columns()),
        Ok(vec![unassigned]),
        Ok(vec![context()]),
    ]);
    let (rows, coverage) = collect_from(&mut reader).await;
    let rows = rows.unwrap().observations;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].request.is_none() && rows[0].scope.is_none());
    assert_eq!(rows[0].granted_kibibytes.as_deref(), Some("65536"));
    assert_eq!(state(&coverage), SourceState::Partial);
    assert_eq!(reader.query_count(), 5);
}

#[tokio::test]
async fn an_unconfirmed_shutdown_discards_staged_grants_and_terminal_reads_never_run_verification() {
    let mut uncertain = successful();
    set_shutdown(&mut uncertain, SourceShutdown::Unconfirmed);
    let (rows, coverage) = collect_from(&mut uncertain).await;
    assert!(rows.is_none());
    assert_eq!(state(&coverage), SourceState::Unavailable);
    assert_eq!(closed(&uncertain), 1);
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::ByteLimit] {
        let mut reader = reader(vec![
            Ok(vec![crate::grants::tests::platform()]),
            Ok(vec![context()]),
            Ok(columns()),
            Err(crate::read::ReadError::Stopped(stop)),
        ]);
        let (rows, coverage) = collect_from(&mut reader).await;
        assert!(rows.is_none());
        assert_eq!(state(&coverage), SourceState::Unavailable);
        assert_eq!(reader.query_count(), 4);
        assert_eq!(closed(&reader), 1);
    }
}

fn boxed_reader(caller: &CollectionBudget) -> ActivityReader<crate::resources::test_support::FakeRead> {
    use crate::{
        activity::{ReadOptions, tests::details},
        instance::test_support::{epoch, metadata},
        resources::test_support::FakeRead,
    };
    let budget = bounded_budget(caller);
    let authority = crate::activity::database_context::tests::row(3);
    let native = replace(&row(), &[("edition", Val::Int(3))]);
    let replies = vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority.clone()]),
        Ok(columns()),
        Ok(vec![native]),
        Ok(vec![details(42, "SELECT payload FROM dbo.orders WHERE id=42")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority]),
    ];
    reader_with_options(
        FakeRead::new(replies).with_query_limit(budget.max_queries),
        ReadOptions { budget, identity: IdentityDetail::Endpoint },
    )
}

#[tokio::test]
async fn the_boxed_profile_finishes_at_nine_native_phases_without_replacing_its_owner() {
    let mut reader = boxed_reader(&CollectionBudget::default());
    let (capture, coverage) = collect_from(&mut reader).await;
    let capture = capture.unwrap();
    assert!(capture.is_valid());
    assert!(capture.context.has_qualified_local_owner());
    assert_eq!(capture.observations.len(), 1);
    assert_eq!(reader.query_count(), MAX_QUERIES);
    assert_eq!(MAX_QUERIES, 9);
    assert_eq!(closed(&reader), 1);
    assert_eq!(state(&coverage), SourceState::Collected);
}

#[tokio::test]
async fn a_stricter_query_budget_cannot_be_reset_to_make_room_for_the_final_owner_check() {
    let caller = CollectionBudget { max_queries: 8, ..Default::default() };
    let mut reader = boxed_reader(&caller);
    let (capture, coverage) = collect_from(&mut reader).await;
    assert!(capture.is_none());
    assert_eq!(reader.query_count(), 8);
    assert_eq!(state(&coverage), SourceState::Unavailable);
    assert_eq!(closed(&reader), 1);
    assert!(coverage.outcomes().iter().any(|source| source.reason.as_ref().is_some_and(|reason| !reason.is_empty())));
}

#[tokio::test]
async fn cancellation_during_owner_acquisition_never_reaches_native_grants_or_a_final_probe() {
    let mut reader = reader_with_options(
        crate::resources::test_support::FakeRead::new(vec![Err(crate::read::ReadError::Stopped(
            CollectionStop::Cancelled,
        ))]),
        ReadOptions { budget: bounded_budget(&CollectionBudget::default()), identity: IdentityDetail::Endpoint },
    );
    let (capture, coverage) = collect_from(&mut reader).await;
    assert!(capture.is_none());
    assert_eq!(state(&coverage), SourceState::Unavailable);
    assert_eq!(reader.query_count(), 1);
    assert_eq!(closed(&reader), 1);
}
