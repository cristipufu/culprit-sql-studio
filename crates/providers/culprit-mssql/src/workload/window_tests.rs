use super::*;
use crate::{
    query_store::{
        Reader,
        tests::summary_row,
        window_sql::tests::{layout_row, open_row, row},
    },
    read::{NativeRead, ReadError},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    SourceState, source_id,
    telemetry::{CollectionStop, SourceShutdown, WaitCaptureState},
};

#[tokio::test]
async fn current_wait_mode_changes_retain_valid_runtime_and_only_qualify_wait_source_coverage() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![row()]),
        Ok(vec![summary_row()]),
        Ok(vec![replace(&row(), &[("wait_stats_capture_mode_desc", Val::Text("OFF".into()))])]),
    ]));
    let runtime = reader.summary(60).await.unwrap();
    let mut coverage = Coverage::default();
    verify_runtime_window(&mut reader, &mut coverage).await.unwrap();
    assert_eq!(runtime[0].moments.executions, 2);
    assert_eq!(runtime[0].moments.duration_us, 4000.0);
    let window = reader.window(60).await.unwrap();
    assert!(window.retained && window.end.is_some() && window.hours == 1.0);
    assert_eq!(window.capture, WaitCaptureState::Disabled);
    let outcomes = coverage.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].source, source_id::QUERY_WAITS);
    assert_eq!(outcomes[0].state, SourceState::Partial);
    assert_eq!(reader.queries(), 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn missing_native_runtime_membership_is_an_error_and_stops_further_statistics() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![row()]),
        Ok(vec![replace(&row(), &[("intervals_retained", Val::Int(0))])]),
        Ok(vec![]),
    ]));
    reader.window(60).await.unwrap();
    let mut coverage = Coverage::default();
    let error = verify_runtime_window(&mut reader, &mut coverage).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(coverage.outcomes().is_empty(), "an invalid runtime population is not usable partial statistics");
    assert!(reader.window(60).await.is_err());
    assert!(reader.summary(60).await.is_err());
    assert!(NativeRead::query(&mut reader, "SELECT /* culprit */ 1", vec![], 1).await.is_err());
    assert_eq!(reader.queries(), 3);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn verification_failures_propagate_without_followup_or_a_stale_denominator() {
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut native =
            FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![row()]), Err(ReadError::Stopped(stop)), Ok(vec![])]);
        if stop == CollectionStop::CancellationUnconfirmed {
            native.shutdown = SourceShutdown::Unconfirmed;
        }
        let expected_shutdown = native.shutdown;
        let mut reader = Reader::new(native);
        reader.window(60).await.unwrap();
        let error = verify_runtime_window(&mut reader, &mut Coverage::default()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert!(reader.window(60).await.is_err());
        assert!(reader.summary(60).await.is_err());
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.close().await, expected_shutdown);
    }
}

#[tokio::test]
async fn final_runtime_verification_keeps_native_subsecond_bounds_for_rate_denominators() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![open_row("2020-01-01T06:00:00.0500000Z", 0.05)]),
        Ok(vec![open_row("2020-01-01T06:00:00.2000000Z", 0.2)]),
    ]));
    reader.window(60).await.unwrap();
    let mut coverage = Coverage::default();
    verify_runtime_window(&mut reader, &mut coverage).await.unwrap();
    let window = reader.window(60).await.unwrap();
    assert_eq!(window.end.as_deref(), Some("2020-01-01T06:00:00.2000000Z"));
    assert_eq!(window.hours, 0.2 / 3600.0);
    assert!(coverage.outcomes().is_empty());
    assert_eq!(reader.queries(), 3);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
