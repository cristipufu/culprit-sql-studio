use super::*;
use crate::{
    query_store::window_sql::{
        WindowVerification,
        tests::{layout_row, open_row, row},
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

#[tokio::test]
async fn runtime_waits_and_display_use_one_bound_native_population_without_a_second_rolling_clock() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![open_row("2020-01-01T06:00:00Z", 0.0)]),
        Ok(vec![super::super::tests::summary_row()]),
        Ok(vec![Rec::of(&[
            ("wait_category", Val::Int(3)),
            ("wait_category_desc", Val::Text("Lock".into())),
            ("wait_ms", Val::Int(40)),
        ])]),
        Ok(vec![open_row("2020-01-01T06:00:03Z", 3.0)]),
    ]));
    let summary = reader.summary(60).await.unwrap();
    assert_eq!(summary.len(), 1);
    let window = reader.window(60).await.unwrap();
    assert_eq!(reader.queries(), 3, "display reuses the native metadata observation");
    let waits = reader.waits(60, None).await.unwrap();
    assert_eq!(waits[0].category_id, 3);
    assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::Unchanged);
    let observed_until = reader.window(60).await.unwrap().end.unwrap();
    let last_execution = summary[0].last_seen.as_ref().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(&observed_until).unwrap()
            >= chrono::DateTime::parse_from_rfc3339(last_execution).unwrap()
    );
    assert_eq!(observed_until, "2020-01-01T06:00:03Z");
    assert_eq!(reader.queries(), 5);
    let native_scope = window.parameter();
    for index in [2, 3] {
        let query = &reader.io.calls[index];
        assert!(query.sql.contains(super::super::window_sql::MEMBERSHIP));
        assert!(!query.sql.contains("SYSDATETIMEOFFSET()"));
        assert!(
            matches!((&query.parameters[0], &native_scope), (Parameter::Text(actual),Parameter::Text(expected)) if actual == expected)
        );
    }
    assert!(reader.window(120).await.is_err());
    assert_eq!(reader.queries(), 5, "a different window cannot silently change a reader's population");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn native_subsecond_recheck_does_not_round_the_endpoint_before_an_observed_execution() {
    let runtime = replace(
        &super::super::tests::summary_row(),
        &[
            ("first_seen", Val::Text("2020-01-01T06:00:00.1000000Z".into())),
            ("last_seen", Val::Text("2020-01-01T06:00:00.1000000Z".into())),
        ],
    );
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![open_row("2020-01-01T06:00:00.0500000Z", 0.05)]),
        Ok(vec![runtime]),
        Ok(vec![open_row("2020-01-01T06:00:00.2000000Z", 0.2)]),
    ]));
    let runtime = reader.summary(60).await.unwrap().remove(0);
    assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::Unchanged);
    let window = reader.window(60).await.unwrap();
    assert_eq!(window.end.as_deref(), Some("2020-01-01T06:00:00.2000000Z"));
    assert_eq!(window.hours, 0.2 / 3600.0);
    assert!(
        chrono::DateTime::parse_from_rfc3339(window.end.as_deref().unwrap()).unwrap()
            > chrono::DateTime::parse_from_rfc3339(runtime.last_seen.as_deref().unwrap()).unwrap()
    );
    assert_eq!(reader.queries(), 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

fn with_recheck(recheck: ReadResult<Vec<Rec>>) -> Reader<FakeRead> {
    Reader::new(FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![row()]), recheck, Ok(vec![])]))
}

async fn assert_no_followup_sql(reader: &mut Reader<FakeRead>) {
    let queries = reader.queries();
    let window = reader.window.as_ref().unwrap();
    assert!(!window.retained && window.end.is_none() && window.hours == 0.0);
    assert!(reader.window(60).await.is_err());
    assert!(reader.summary(60).await.is_err());
    assert!(reader.waits(60, None).await.is_err());
    assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
    assert!(NativeRead::query(&mut *reader, "SELECT /* culprit */ 2", vec![], 1).await.is_err());
    assert_eq!(reader.queries(), queries, "a retired native window admits no scoped or raw SQL");
}

#[tokio::test]
async fn failed_and_terminal_rechecks_withdraw_cached_bounds_and_fence_followup_reads() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut reader = with_recheck(Err(ReadError::Stopped(stop)));
        if stop == CollectionStop::CancellationUnconfirmed {
            reader.io.shutdown = SourceShutdown::Unconfirmed;
        }
        reader.window(60).await.unwrap();
        let error = reader.verify_window().await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_no_followup_sql(&mut reader).await;
        let expected = reader.io.shutdown;
        assert_eq!(reader.close().await, expected);
        assert_eq!(reader.io.closed, 1);
    }
    for rows in [vec![], vec![row(), row()]] {
        let mut reader = with_recheck(Ok(rows));
        reader.window(60).await.unwrap();
        assert!(reader.verify_window().await.is_err(), "a malformed metadata reply does not leave a trusted endpoint");
        assert_no_followup_sql(&mut reader).await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn vanished_native_membership_retires_the_window_but_current_mode_changes_preserve_runtime() {
    let mut reader = with_recheck(Ok(vec![replace(&row(), &[("intervals_retained", Val::Int(0))])]));
    reader.window(60).await.unwrap();
    assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::RuntimePopulationChanged);
    assert_no_followup_sql(&mut reader).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let mut reader =
        with_recheck(Ok(vec![replace(&row(), &[("wait_stats_capture_mode_desc", Val::Text("OFF".into()))])]));
    reader.window(60).await.unwrap();
    assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::WaitCaptureChanged);
    let window = reader.window(60).await.unwrap();
    assert!(window.retained && window.end.is_some() && window.hours == 1.0);
    assert_eq!(window.capture, culprit_core::telemetry::WaitCaptureState::Disabled);
    assert!(reader.read_rows("SELECT /* culprit */ 1").await.unwrap().is_empty());
    assert_eq!(reader.queries(), 4, "current wait configuration does not retire a stable runtime population");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
