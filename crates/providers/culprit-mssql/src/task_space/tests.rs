use super::*;
use crate::activity::{
    memory::tests::{closed, set_shutdown},
    tempdb::tests::{reader, replies, task},
};

#[tokio::test]
async fn unrequested_task_space_never_connects_or_runs_native_sql() {
    let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
    let (snapshot, coverage) = collect(pool, false, "unused.invalid".into(), 1433).await;
    assert!(snapshot.is_none());
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::NotApplicable);
}

#[tokio::test]
async fn successful_native_task_rows_remain_provisional_until_confirmed_close() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let mut reader = reader(replies(vec![task(0)]));
        set_shutdown(&mut reader, shutdown);
        let (snapshot, coverage) = collect_from(&mut reader).await;
        assert_eq!(closed(&reader), 1);
        assert_eq!(snapshot.is_some(), shutdown == SourceShutdown::Quiescent);
        assert_eq!(
            coverage.outcomes()[0].state,
            if shutdown == SourceShutdown::Quiescent {
                culprit_core::SourceState::Collected
            } else {
                culprit_core::SourceState::Unavailable
            }
        );
    }
}
