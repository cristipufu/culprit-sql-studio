use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    Error, ErrorCode, MeasurementScope, ResetMarker, SourceState,
    telemetry::{CollectionStop, ObservationReadContext, SourceShutdown},
};

#[tokio::test]
async fn the_existing_wait_read_retains_benign_nondominant_exact_inputs_without_another_query() {
    for azure in [false, true] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![
                replace(
                    &crate::waits::tests::row("WRITELOG", 9_007_199_254_740_993),
                    &[("waiting_tasks_count", Val::Int(9_007_199_254_740_993))],
                ),
                crate::waits::tests::row("LOGBUFFER", 3),
                crate::waits::tests::row("SLEEP_TASK", 2),
                crate::waits::tests::row("BACKGROUND_NATIVE_WAIT", 1),
            ]),
            Ok(lifetimes::native_counters("SQLServer")),
        ]);
        let history = history_from(&mut reader, azure).await;
        let context = history.wait_context.clone().unwrap();
        let mut probe = lifetimes::probe(Some("2000-01-01T00:00:00Z"));
        probe.engine_edition = if azure { 5 } else { 3 };
        let captured = project_history(&probe, history);
        let read = captured.pressure.wait_counters.as_ref().unwrap();
        assert!(read.is_valid_for(Engine::Mssql));
        assert_eq!(read.context.as_ref(), Some(&context));
        assert!(matches!(context, ObservationReadContext::Collector { .. }));
        assert_eq!(read.sources[0].records, 4);
        assert_eq!(read.observations.len(), 4);
        assert_eq!(read.observations[0].wait_time_ms, "9007199254740993");
        assert_eq!(read.observations[0].waiting_tasks_count, "9007199254740993");
        assert!(read.observations.iter().all(|row| row.reset == ResetMarker::Unknown));
        assert!(
            read.observations
                .iter()
                .all(|row| row.scope == if azure { MeasurementScope::Database } else { MeasurementScope::Server })
        );
        assert!(captured.waits.iter().all(|category| category.wait_type.as_deref() != Some("SLEEP_TASK")));
        assert_eq!(
            captured.waits.iter().find(|category| category.category == "Log").unwrap().wait_type.as_deref(),
            Some("WRITELOG")
        );
        assert!(read.observations.iter().any(|row| row.wait_type == "LOGBUFFER"));
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(reader.closed, 1);
        assert_eq!(reader.calls[0].limit, crate::waits::MAX_WAIT_TYPES);
    }
}

#[tokio::test]
async fn permissions_empty_and_unconfirmed_shutdown_keep_independent_truthful_source_states() {
    let denied = Error::new(ErrorCode::PermissionDenied, "fixture denied");
    for (rows, shutdown, expected) in [
        (Ok(vec![]), SourceShutdown::Quiescent, SourceState::Empty),
        (Err(ReadError::Source(denied)), SourceShutdown::Quiescent, SourceState::Unavailable),
        (Ok(vec![crate::waits::tests::row("WRITELOG", 10)]), SourceShutdown::Unconfirmed, SourceState::Unavailable),
    ] {
        let mut reader = FakeRead::new(vec![rows, Ok(lifetimes::native_counters("SQLServer"))]);
        reader.shutdown = shutdown;
        let history = history_from(&mut reader, false).await;
        let captured = project_history(&lifetimes::probe(None), history);
        let read = captured.pressure.wait_counters.unwrap();
        assert!(read.is_valid_for(Engine::Mssql));
        assert_eq!(read.sources[0].state, expected);
        assert!(read.observations.is_empty());
        assert_eq!(read.sources[0].records, 0);
        assert!(read.read_complete);
        assert_eq!(reader.closed, 1);
        if shutdown == SourceShutdown::Unconfirmed {
            assert_eq!(read.sources[0].stopped, Some(CollectionStop::CancellationUnconfirmed));
        }
    }
}

#[tokio::test]
async fn terminal_wait_stops_do_not_send_followup_counter_queries_or_publish_partial_success() {
    for reason in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::SourceChanged,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(reason))]);
        let history = history_from(&mut reader, false).await;
        let captured = project_history(&lifetimes::probe(None), history);
        let read = captured.pressure.wait_counters.unwrap();
        assert_eq!(read.sources[0].state, SourceState::Unavailable);
        assert_eq!(read.sources[0].stopped, Some(reason));
        assert!(read.observations.is_empty());
        assert_eq!(reader.calls.len(), 1, "{reason:?}");
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn an_inconsistent_original_total_is_retained_and_short_sample_reset_guards_are_unchanged() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![crate::waits::tests::row("WRITELOG", 1)]),
        Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "counter denied"))),
    ]);
    let history = history_from(&mut reader, false).await;
    let captured = project_history(&lifetimes::probe(None), history);
    let read = captured.pressure.wait_counters.unwrap();
    assert_eq!(read.observations[0].wait_time_ms, "1");
    assert_eq!(read.observations[0].signal_wait_time_ms, "2");
    assert_eq!(read.observations[0].resource_wait_time_ms(), None);
    assert!(read.is_valid_for(Engine::Mssql));
    let mut changed = read.observations.clone();
    changed[0].reset = ResetMarker::Generation("changed-native-counter-generation".into());
    assert_eq!(
        CollectionStop::from_error(&waits::deltas(&read.observations, &changed).unwrap_err()),
        CollectionStop::SourceChanged
    );
    assert_eq!(reader.calls.len(), 2);
}
