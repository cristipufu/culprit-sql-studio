use super::*;
use crate::{
    resources::test_support::{instance_pressure_row, replace},
    value::Val,
};

fn native_shared_point(row: crate::value::Rec) -> FakeRead {
    FakeRead::new(vec![Ok(vec![memory_row()]), Ok(fixture_columns()), Ok(vec![scheduler_row()]), Ok(vec![row])])
}

#[tokio::test]
async fn high_native_flags_and_waiters_are_retained_without_invoking_optional_read_admission() {
    let native = replace(
        &instance_pressure_row(),
        &[
            ("process_physical_low", Val::Int(1)),
            ("grant_rows", Val::Int(1)),
            ("grant_waiters", Val::Int(1)),
            ("process_physical_used_kib", Val::Int(9007199254740993)),
            ("process_virtual_committed_kib", Val::Int(0)),
            ("process_virtual_available_kib", Val::Null),
        ],
    );
    let mut reader = native_shared_point(native);
    let read = collect_from(&mut reader, true, true, true, None).await;
    let point = read.instance_resources.unwrap();
    assert!(point.is_valid());
    let raw = point.observation.unwrap();
    assert_eq!(raw.memory.process_physical_low, Some(true));
    assert_eq!(raw.grant_waiters, 1);
    assert_eq!(raw.memory.physical_used_kib.as_deref(), Some("9007199254740993"));
    assert_eq!(raw.memory.virtual_committed_kib.as_deref(), Some("0"));
    assert_eq!(raw.memory.virtual_available_kib, None);
    assert!(matches!(point.context, ObservationReadContext::Collector { .. }));
    assert_eq!(read.counters.unwrap().len(), 2);
    assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::Collected);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.calls[3].limit, 1);
    assert!(reader.calls[3].sql.contains("sys.dm_os_process_memory"));
    assert!(!reader.calls[3].sql.contains("sys.dm_exec_sql_text"));
}

#[tokio::test]
async fn native_truncated_prefix_is_partial_with_accessible_values_and_independent_counter_sets() {
    let row = replace(
        &instance_pressure_row(),
        &[
            ("scheduler_population_rows", Val::Int(1025)),
            ("runnable_tasks", Val::Int(4)),
            ("system_memory_low", Val::Null),
            ("system_memory_invalid", Val::Int(1)),
        ],
    );
    let mut reader = native_shared_point(row);
    let read = collect_from(&mut reader, true, true, true, None).await;
    let point = read.instance_resources.unwrap();
    assert!(point.is_valid());
    assert_eq!(point.source.state, SourceState::Partial);
    assert!(!point.source.complete_scope);
    assert_eq!(point.observation.as_ref().unwrap().schedulers.population_rows, 1025);
    assert_eq!(point.observation.as_ref().unwrap().schedulers.runnable_tasks.as_deref(), Some("4"));
    assert_eq!(read.counters.unwrap().len(), 2);
    assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::Partial);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn denied_or_unsupported_instance_point_never_erases_independent_shared_counters_or_adds_a_retry() {
    let mut denied = FakeRead::new(vec![
        Ok(vec![memory_row()]),
        Ok(fixture_columns()),
        Ok(vec![scheduler_row()]),
        Err(Error::new(ErrorCode::PermissionDenied, "controlled instance point denial").into()),
    ]);
    let read = collect_from(&mut denied, true, true, true, Some("controlled native grant")).await;
    assert!(read.instance_resources.is_none());
    assert_eq!(read.counters.unwrap().len(), 2);
    assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::Unavailable);
    assert_eq!((denied.calls.len(), denied.closed), (4, 1));
    let mut unsupported = successful();
    let read = collect_from(&mut unsupported, true, true, false, None).await;
    assert!(read.instance_resources.is_none());
    assert_eq!(read.counters.unwrap().len(), 2);
    assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::NotApplicable);
    assert_eq!((unsupported.calls.len(), unsupported.closed), (3, 1));
}

#[tokio::test]
async fn terminal_source_work_and_unconfirmed_final_close_cannot_publish_staged_points_or_start_replacements() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::SourceChanged,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut reader = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(stop))]);
        let read = collect_from(&mut reader, true, true, true, None).await;
        assert!(read.counters.is_none() && read.instance_resources.is_none());
        assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::Unavailable);
        assert_eq!((reader.calls.len(), reader.closed), (1, 1));
    }
    let mut reader = native_shared_point(instance_pressure_row());
    reader.shutdown = SourceShutdown::Unconfirmed;
    let read = collect_from(&mut reader, true, true, true, None).await;
    assert!(read.counters.is_none() && read.instance_resources.is_none());
    assert_eq!(state(&read.coverage, INSTANCE_RESOURCES_SOURCE), SourceState::Unavailable);
    assert_eq!((reader.calls.len(), reader.closed), (4, 1));
}
