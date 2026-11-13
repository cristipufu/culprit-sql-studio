use super::*;
use crate::{
    activity::memory::tests::{closed, columns, context, reader, row, set_shutdown, successful},
    resources::test_support::replace,
    value::Val,
};
use culprit_core::SourceState;

fn state(coverage: &Coverage) -> SourceState {
    coverage.outcomes().into_iter().find(|source| source.source == sources::CURRENT_REQUEST_MEMORY.name).unwrap().state
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
    assert_eq!(reader.query_count(), MAX_SCAN_QUERIES);
    assert_eq!(closed(&reader), 1);
}

#[tokio::test]
async fn unassigned_grants_remain_visible_as_partial_evidence_without_a_database_measurement() {
    let unassigned = replace(&row(), &[("request_generation", Val::Text("2020-01-01T07:59:00".into()))]);
    let mut reader = reader(vec![Ok(vec![context()]), Ok(columns()), Ok(vec![unassigned]), Ok(vec![context()])]);
    let (rows, coverage) = collect_from(&mut reader).await;
    let rows = rows.unwrap().observations;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].request.is_none() && rows[0].scope.is_none());
    assert_eq!(rows[0].granted_kibibytes.as_deref(), Some("65536"));
    assert_eq!(state(&coverage), SourceState::Partial);
    assert_eq!(reader.query_count(), 4);
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
        let mut reader = reader(vec![Ok(vec![context()]), Ok(columns()), Err(crate::read::ReadError::Stopped(stop))]);
        let (rows, coverage) = collect_from(&mut reader).await;
        assert!(rows.is_none());
        assert_eq!(state(&coverage), SourceState::Unavailable);
        assert_eq!(reader.query_count(), 3);
        assert_eq!(closed(&reader), 1);
    }
}
