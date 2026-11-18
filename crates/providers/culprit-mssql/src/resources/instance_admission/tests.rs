use super::super::admit_instance_resources;
use super::*;
use crate::resources::test_support::{FakeRead, instance_pressure_row, replace};

#[tokio::test]
async fn healthy_native_instance_point_uses_one_read_without_azure_percentages_or_retry() {
    let mut reader = FakeRead::new(vec![Ok(vec![instance_pressure_row()])]);
    let point = super::super::read_instance_pressure(&mut reader).await.unwrap();
    assert_eq!(point.observed_at, "2026-11-18T12:00:00.000000000Z");
    assert!(point.is_complete());
    assert!(point.admit().is_ok());
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, 1);
    assert!(reader.calls[0].parameters.is_empty());
    assert_eq!(reader.closed, 0, "the enclosing original source retains native shutdown ownership");
    let sql = &reader.calls[0].sql;
    for native in [
        "sys.dm_os_schedulers",
        "sys.dm_os_process_memory",
        "sys.dm_os_sys_memory",
        "sys.dm_exec_query_memory_grants",
        "sys.dm_io_pending_io_requests",
        "sys.dm_os_waiting_tasks",
    ] {
        assert_eq!(sql.matches(native).count(), 1, "one bounded population per native DMV");
    }
    for forbidden in [
        "sys.dm_db_resource_stats",
        "io_pending_ms_ticks",
        "sys.dm_exec_sql_text",
        "sys.dm_exec_query_plan",
        "ORDER BY",
        "COALESCE",
        "ISNULL",
    ] {
        assert!(!sql.contains(forbidden));
    }
    assert!(sql.contains("TOP(1025)") && sql.matches("TOP(257)").count() == 3 && sql.matches("TOP(2)").count() == 2);
    assert!(sql.ends_with("OPTION(MAXDOP 1)"));
}

#[tokio::test]
async fn every_native_pressure_dimension_refuses_optional_work_independently() {
    for changes in [
        vec![("runnable_tasks", Val::Int(2))],
        vec![("worker_queue", Val::Int(1))],
        vec![("failed_worker_creations", Val::Int(1))],
        vec![("process_physical_low", Val::Int(1))],
        vec![("process_virtual_low", Val::Int(1))],
        vec![("system_memory_low", Val::Int(1))],
        vec![("grant_rows", Val::Int(1)), ("grant_waiters", Val::Int(1))],
        vec![("pending_io_rows", Val::Int(32)), ("os_pending_io", Val::Int(32))],
        vec![
            ("io_waiter_population_rows", Val::Int(1)),
            ("io_waiter_rows", Val::Int(1)),
            ("longest_io_related_wait_ms", Val::Int(1000)),
        ],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&instance_pressure_row(), &changes)])]);
        assert!(matches!(
            admit_instance_resources(&mut reader).await,
            Err(ReadError::Stopped(CollectionStop::Pressure))
        ));
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn documented_wait_and_os_pending_counts_keep_their_independent_thresholds() {
    let row = replace(
        &instance_pressure_row(),
        &[
            ("runnable_tasks", Val::Int(1)),
            ("pending_io_rows", Val::Int(31)),
            ("os_pending_io", Val::Int(31)),
            ("io_waiter_population_rows", Val::Int(1)),
            ("io_waiter_rows", Val::Int(1)),
            ("longest_io_related_wait_ms", Val::Int(999)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    assert!(admit_instance_resources(&mut reader).await.is_ok());
    // OS-completed work awaiting SQL completion is not a native storage-pending queue or inferred I/O age.
    let row = replace(
        &instance_pressure_row(),
        &[("pending_io_rows", Val::Int(32)), ("completion_pending_io", Val::Int(32))],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    let point = read_instance_pressure(&mut reader).await.unwrap();
    assert_eq!(point.pending_io.os_pending, Some(0));
    assert_eq!(point.pending_io.completion_pending, Some(32));
    assert!(point.admit().is_ok());
}

#[tokio::test]
async fn bounded_overflow_and_missing_native_populations_cannot_be_admitted() {
    for changes in [
        vec![("scheduler_population_rows", Val::Int(1025))],
        vec![("scheduler_rows", Val::Int(0))],
        vec![
            ("process_memory_rows", Val::Int(0)),
            ("process_physical_low", Val::Null),
            ("process_virtual_low", Val::Null),
            ("process_physical_used_kib", Val::Null),
            ("process_virtual_committed_kib", Val::Null),
            ("process_virtual_available_kib", Val::Null),
        ],
        vec![
            ("process_memory_rows", Val::Int(2)),
            ("process_physical_low", Val::Null),
            ("process_virtual_low", Val::Null),
            ("process_physical_used_kib", Val::Null),
            ("process_virtual_committed_kib", Val::Null),
            ("process_virtual_available_kib", Val::Null),
        ],
        vec![
            ("system_memory_rows", Val::Int(0)),
            ("system_memory_low", Val::Null),
            ("system_total_kib", Val::Null),
            ("system_available_kib", Val::Null),
        ],
        vec![
            ("system_memory_rows", Val::Int(2)),
            ("system_memory_low", Val::Null),
            ("system_total_kib", Val::Null),
            ("system_available_kib", Val::Null),
        ],
        vec![("grant_rows", Val::Int(257)), ("grant_waiters", Val::Int(257))],
        vec![("pending_io_rows", Val::Int(257)), ("completion_pending_io", Val::Int(257))],
        vec![
            ("io_waiter_population_rows", Val::Int(257)),
            ("io_waiter_rows", Val::Int(257)),
            ("longest_io_related_wait_ms", Val::Int(1)),
        ],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&instance_pressure_row(), &changes)])]);
        let point = read_instance_pressure(&mut reader).await.unwrap();
        assert!(!point.is_complete());
        assert_eq!(point.admit(), Err(CollectionStop::PressureUnavailable));
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn native_null_flags_and_partial_summaries_remain_unknown_with_accessible_positives() {
    for changes in [
        vec![("failed_worker_creations", Val::Null), ("scheduler_invalid", Val::Int(1))],
        vec![("runnable_tasks", Val::Null), ("scheduler_invalid", Val::Int(1))],
        vec![("worker_queue", Val::Null), ("scheduler_invalid", Val::Int(1))],
        vec![("process_physical_low", Val::Null), ("process_memory_invalid", Val::Int(1))],
        vec![("process_virtual_low", Val::Null), ("process_memory_invalid", Val::Int(1))],
        vec![("system_memory_low", Val::Null), ("system_memory_invalid", Val::Int(1))],
        vec![
            ("pending_io_rows", Val::Int(1)),
            ("pending_io_invalid", Val::Int(1)),
            ("os_pending_io", Val::Null),
            ("completion_pending_io", Val::Null),
        ],
        vec![
            ("io_waiter_population_rows", Val::Int(1)),
            ("io_waiter_rows", Val::Int(1)),
            ("io_waiter_unknown_durations", Val::Int(1)),
            ("longest_io_related_wait_ms", Val::Null),
        ],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&instance_pressure_row(), &changes)])]);
        let point = read_instance_pressure(&mut reader).await.unwrap();
        assert_eq!(point.memory.physical_used_kib, Some(32768));
        assert_eq!(point.admit(), Err(CollectionStop::PressureUnavailable));
    }
}

#[tokio::test]
async fn optional_native_memory_detail_keeps_exact_large_zero_and_null_without_replacing_flags() {
    let row = replace(
        &instance_pressure_row(),
        &[
            ("process_physical_used_kib", Val::Int(9007199254740993)),
            ("process_virtual_committed_kib", Val::Int(0)),
            ("process_virtual_available_kib", Val::Null),
            ("system_total_kib", Val::Null),
            ("system_available_kib", Val::Int(0)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    let point: super::super::InstancePressurePoint = read_instance_pressure(&mut reader).await.unwrap();
    assert_eq!(point.memory.physical_used_kib, Some(9007199254740993));
    assert_eq!(point.memory.virtual_committed_kib, Some(0));
    assert_eq!(point.memory.virtual_available_kib, None);
    assert_eq!(point.memory.system_total_kib, None);
    assert_eq!(point.memory.system_available_kib, Some(0));
    assert!(point.admit().is_ok());
    let row = replace(
        &instance_pressure_row(),
        &[
            ("process_physical_used_kib", Val::Null),
            ("process_virtual_committed_kib", Val::Null),
            ("process_virtual_available_kib", Val::Null),
            ("system_total_kib", Val::Null),
            ("system_available_kib", Val::Null),
            ("system_memory_low", Val::Int(1)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    assert!(matches!(admit_instance_resources(&mut reader).await, Err(ReadError::Stopped(CollectionStop::Pressure))));
}

#[tokio::test]
async fn malformed_shape_types_clock_or_incoherent_native_counts_fail_closed() {
    for changes in [
        vec![("observed_at", Val::Null)],
        vec![("observed_at", Val::Text("not-a-native-clock".into()))],
        vec![("runnable_tasks", Val::Int(-1))],
        vec![("runnable_tasks", Val::Float(0.0))],
        vec![("runnable_tasks", Val::Text("0".into()))],
        vec![("system_memory_low", Val::Int(2))],
        vec![("process_physical_used_kib", Val::Int(-1))],
        vec![("scheduler_rows", Val::Int(1026))],
        vec![("process_memory_rows", Val::Int(3))],
        vec![("failed_worker_creations", Val::Int(3))],
        vec![("pending_io_rows", Val::Int(1))],
        vec![("longest_io_related_wait_ms", Val::Int(1))],
        vec![("io_waiter_unknown_durations", Val::Int(1))],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&instance_pressure_row(), &changes)])]);
        assert!(matches!(
            admit_instance_resources(&mut reader).await,
            Err(ReadError::Stopped(CollectionStop::PressureUnavailable))
        ));
        assert_eq!(reader.calls.len(), 1);
    }
    let fields = instance_pressure_row();
    let missing = Rec::of(
        &fields
            .columns()
            .filter(|(name, _)| *name != "process_physical_low")
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    );
    for rows in [vec![], vec![fields.clone(), fields.clone()], vec![missing]] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(matches!(
            admit_instance_resources(&mut reader).await,
            Err(ReadError::Stopped(CollectionStop::PressureUnavailable))
        ));
    }
}

#[tokio::test]
async fn denied_missing_and_all_lifecycle_stops_remain_fail_closed_without_followup_or_close() {
    let denied =
        culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "controlled native visibility denial");
    let mut reader = FakeRead::new(vec![Err(ReadError::Source(denied))]);
    assert!(
        matches!(admit_instance_resources(&mut reader).await,Err(ReadError::Source(error)) if error.code==culprit_core::ErrorCode::PermissionDenied)
    );
    assert_eq!(reader.calls.len(), 1);
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::SourceChanged,
    ] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        assert!(matches!(admit_instance_resources(&mut reader).await,Err(ReadError::Stopped(actual)) if actual==stop));
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 0);
    }
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::RowLimit))]);
    assert!(matches!(
        admit_instance_resources(&mut reader).await,
        Err(ReadError::Stopped(CollectionStop::PressureUnavailable))
    ));
}

#[test]
fn native_population_caps_precede_all_classification_and_aggregation() {
    let query = sql::query();
    assert!(!query.contains("WHERE"), "a filtered native census can scan unlimited unrelated rows before TOP");
    for raw_population in [
        "FROM (SELECT TOP(1025) status,is_online,runnable_tasks_count,work_queue_count,failed_to_create_worker FROM sys.dm_os_schedulers) native",
        "FROM (SELECT TOP(257) grant_time FROM sys.dm_exec_query_memory_grants) native",
        "FROM (SELECT TOP(257) wait_type,wait_duration_ms FROM sys.dm_os_waiting_tasks) native",
    ] {
        assert!(query.contains(raw_population), "TOP must apply to the raw native population before classification");
    }
    assert!(
        query
            .contains("COUNT_BIG(*) AS grant_rows,COUNT_BIG(CASE WHEN grant_time IS NULL THEN 1 END) AS grant_waiters")
    );
    assert!(query.contains("COUNT_BIG(*) AS io_waiter_population_rows"));
    assert!(query.contains("COUNT_BIG(*) AS scheduler_population_rows"));
}

#[tokio::test]
async fn a_quiet_matching_prefix_never_hides_an_oversized_full_native_population() {
    for changes in [
        vec![("scheduler_population_rows", Val::Int(1025))],
        vec![("grant_rows", Val::Int(257))],
        vec![("io_waiter_population_rows", Val::Int(257))],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&instance_pressure_row(), &changes)])]);
        let point = read_instance_pressure(&mut reader).await.unwrap();
        assert_eq!(point.grant_waiters, 0);
        assert_eq!(point.io_waits.observed_rows, 0);
        assert_eq!(point.io_waits.longest_wait_ms, None);
        assert!(!point.is_complete());
        assert_eq!(point.admit(), Err(CollectionStop::PressureUnavailable));
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn complete_unrelated_native_populations_keep_independent_matching_counts() {
    let row = replace(
        &instance_pressure_row(),
        &[
            ("scheduler_population_rows", Val::Int(8)),
            ("grant_rows", Val::Int(256)),
            ("io_waiter_population_rows", Val::Int(256)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    let point = read_instance_pressure(&mut reader).await.unwrap();
    assert_eq!(point.schedulers.population_rows, 8);
    assert_eq!(point.schedulers.observed_rows, 2);
    assert_eq!(point.grant_rows, 256);
    assert_eq!(point.grant_waiters, 0);
    assert_eq!(point.io_waits.population_rows, 256);
    assert_eq!(point.io_waits.observed_rows, 0);
    assert!(point.is_complete());
    assert!(point.admit().is_ok());
}

#[tokio::test]
async fn unknown_metadata_in_an_unmatched_native_task_prefix_is_still_unavailable() {
    for unknown in ["io_waiter_unknown_types", "io_waiter_unknown_durations"] {
        let row =
            replace(&instance_pressure_row(), &[("io_waiter_population_rows", Val::Int(1)), (unknown, Val::Int(1))]);
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        let point = read_instance_pressure(&mut reader).await.unwrap();
        assert_eq!(point.io_waits.observed_rows, 0);
        assert_eq!(point.admit(), Err(CollectionStop::PressureUnavailable));
    }
}
