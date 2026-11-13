use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, MeasurementScope, ResetMarker};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("scheduler_id", Val::Int(0)),
        ("parent_node_id", Val::Int(3)),
        ("cpu_id", Val::Int(6)),
        ("status", Val::Text("VISIBLE ONLINE".into())),
        ("is_online", Val::Bool(true)),
        ("is_idle", Val::Bool(false)),
        ("failed_to_create_worker", Val::Null),
        ("current_tasks_count", Val::Int(8)),
        ("runnable_tasks_count", Val::Int(3)),
        ("current_workers_count", Val::Int(12)),
        ("active_workers_count", Val::Int(6)),
        ("work_queue_count", Val::Int(9_007_199_254_740_993)),
        ("pending_disk_io_count", Val::Int(0)),
        ("preemptive_switches_count", Val::Int(72)),
        ("context_switches_count", Val::Int(140)),
        ("idle_switches_count", Val::Int(12)),
        ("load_factor", Val::Int(9)),
        ("yield_count", Val::Int(160)),
        ("last_timer_activity", Val::Int(402101)),
        ("total_cpu_usage_ms", Val::Int(512)),
        ("total_scheduler_delay_ms", Val::Int(2)),
        ("ideal_workers_limit", Val::Int(14)),
    ])
}
fn counter<'a>(sample: &'a NativeCounterSet, name: &str) -> &'a NativeCounterSample {
    sample.counters.iter().find(|counter| counter.key == name).unwrap()
}
fn metadata_without(names: &[&str]) -> Vec<Rec> {
    fields::all_columns().into_iter().filter(|row| !names.contains(&row.string("name").as_str())).collect()
}

#[tokio::test]
async fn scheduler_facts_preserve_native_identity_shared_scope_and_exact_counting_units() {
    let mut reader = FakeRead::new(vec![Ok(fields::all_columns()), Ok(vec![row()])]);
    let samples = read(&mut reader, 10).await.unwrap();
    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!(sample.is_valid());
    assert_eq!(sample.identity.id, "0");
    assert_eq!(sample.identity.qualifiers["native_object"], NATIVE_OBJECT);
    assert_eq!(sample.scope, MeasurementScope::Server);
    assert_eq!(sample.properties["status"], "VISIBLE ONLINE");
    assert_eq!(sample.properties["parent_node_id"], "3");
    assert_eq!(sample.properties["cpu_id"], "6");
    assert_eq!(counter(sample, "work_queue_count").value, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(counter(sample, "pending_disk_io_count").value, MeasurementValue::Integer("0".into()));
    assert_eq!(counter(sample, "runnable_tasks_count").nature, CounterNature::Gauge);
    assert_eq!(counter(sample, "runnable_tasks_count").unit, CounterUnit::Count);
    assert_eq!(counter(sample, "total_cpu_usage_ms").nature, CounterNature::Cumulative);
    assert_eq!(counter(sample, "total_cpu_usage_ms").unit, CounterUnit::Milliseconds);
    assert_eq!(counter(sample, "yield_count").nature, CounterNature::Unknown);
    assert_eq!(counter(sample, "yield_count").unit, CounterUnit::Unknown);
    assert!(
        sample
            .counters
            .iter()
            .all(|counter| counter.scope == MeasurementScope::Server && counter.reset == ResetMarker::Unknown)
    );
    assert!(!sample.properties.contains_key("failed_to_create_worker"));
    assert_eq!(sample.properties["native_null_columns"], "[\"failed_to_create_worker\"]");
    assert!(!sample.properties.contains_key("query_id"));
    assert_eq!(reader.calls.len(), 2);
    assert_eq!(reader.calls[1].limit, 11);
    assert!(reader.calls.iter().all(|call| call.sql.contains("/* culprit */")
        && call.sql.contains("TOP (@P1)")
        && call.sql.contains("MAXDOP 1")));
    assert!(!reader.calls[1].sql.contains("sys.dm_exec_sessions"));
    assert!(!reader.calls[1].sql.contains("quantum_length_us"));
    assert!(!reader.calls[1].sql.contains("total_cpu_idle_capped_ms"));
}

#[tokio::test]
async fn missing_versioned_columns_and_native_nulls_stay_unknown_without_hiding_measured_zero() {
    let absent = ["total_cpu_usage_ms", "ideal_workers_limit"];
    let sample = replace(
        &row(),
        &[
            ("total_cpu_usage_ms", Val::Null),
            ("ideal_workers_limit", Val::Null),
            ("current_tasks_count", Val::Null),
            ("failed_to_create_worker", Val::Bool(false)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(metadata_without(&absent)), Ok(vec![sample])]);
    let sample = read(&mut reader, 10).await.unwrap().remove(0);
    assert!(
        sample
            .counters
            .iter()
            .all(|counter| !absent.contains(&counter.native_name.as_str())
                && counter.native_name != "current_tasks_count")
    );
    assert_eq!(sample.properties["unavailable_columns"], "[\"total_cpu_usage_ms\",\"ideal_workers_limit\"]");
    assert_eq!(sample.properties["native_null_columns"], "[\"current_tasks_count\"]");
    assert_eq!(sample.properties["failed_to_create_worker"], "0");
    assert!(reader.calls[1].sql.contains("CAST(NULL AS bigint) AS [total_cpu_usage_ms]"));
    assert!(reader.calls[1].sql.contains("CAST(NULL AS bigint) AS [ideal_workers_limit]"));
}

#[tokio::test]
async fn scheduler_state_and_counts_do_not_change_identity_or_turn_shared_internal_work_into_application_ownership() {
    let states = [("VISIBLE OFFLINE", false), ("HIDDEN ONLINE", true), ("HIDDEN OFFLINE", false), ("HOT_ADDED", true)];
    let mut original = None;
    for (status, online) in states {
        let sample = replace(
            &row(),
            &[
                ("status", Val::Text(status.into())),
                ("is_online", Val::Bool(online)),
                ("runnable_tasks_count", Val::Int(0)),
                ("failed_to_create_worker", Val::Bool(true)),
            ],
        );
        let mut reader = FakeRead::new(vec![Ok(fields::all_columns()), Ok(vec![sample])]);
        let sample = read(&mut reader, 10).await.unwrap().remove(0);
        assert_eq!(sample.properties["status"], status);
        assert_eq!(sample.properties["is_online"], u8::from(online).to_string());
        assert_eq!(sample.properties["failed_to_create_worker"], "1");
        if let Some(before) = &original {
            assert_eq!(sample.key().unwrap(), *before);
        } else {
            original = Some(sample.key().unwrap());
        }
        assert!(!sample.properties.contains_key("database_id"));
    }
}

#[tokio::test]
async fn metadata_failures_duplicates_and_missing_required_scope_stop_before_the_statistics_query() {
    let mut duplicate = fields::all_columns();
    duplicate[0] = duplicate[1].clone();
    for metadata in [vec![], metadata_without(&[IDENTITY_COLUMN]), duplicate] {
        let mut reader = FakeRead::new(vec![Ok(metadata)]);
        assert!(read(&mut reader, 10).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 10).await.unwrap_err()), CollectionStop::TimedOut);
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn overflow_empty_duplicate_or_invalid_statistics_never_return_partial_counts() {
    for statistics in [
        vec![],
        vec![row(), row()],
        vec![row(), replace(&row(), &[(IDENTITY_COLUMN, Val::Int(1)), ("active_workers_count", Val::Int(-1))])],
        vec![replace(&row(), &[("work_queue_count", Val::Float(1.0))])],
        vec![replace(&row(), &[("is_online", Val::Int(2))])],
        vec![replace(&row(), &[(IDENTITY_COLUMN, Val::Null)])],
    ] {
        let mut reader = FakeRead::new(vec![Ok(fields::all_columns()), Ok(statistics)]);
        assert!(read(&mut reader, 10).await.is_err());
        assert_eq!(reader.calls.len(), 2);
    }
    let mut reader = FakeRead::new(vec![
        Ok(fields::all_columns()),
        Ok(vec![row(), replace(&row(), &[(IDENTITY_COLUMN, Val::Int(1))])]),
    ]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 1).await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(reader.calls[1].limit, 2);
    let mut reader = FakeRead::new(vec![]);
    assert!(read(&mut reader, 0).await.is_err());
    assert!(reader.calls.is_empty());
}

#[tokio::test]
async fn permission_and_transport_errors_do_not_authorize_a_fallback_or_claim_native_shutdown() {
    for failure in [
        ReadError::Source(Error::new(ErrorCode::PermissionDenied, "fixture denied")),
        ReadError::Stopped(CollectionStop::TimedOut),
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
    ] {
        let stopped = match &failure {
            ReadError::Stopped(reason) => Some(*reason),
            ReadError::Source(_) => None,
        };
        let mut reader = FakeRead::new(vec![Ok(fields::all_columns()), Err(failure)]);
        let error = read(&mut reader, 10).await.unwrap_err();
        if let Some(stopped) = stopped {
            assert_eq!(CollectionStop::from_error(&error), stopped);
        } else {
            assert_eq!(error.code, ErrorCode::PermissionDenied);
        }
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(reader.closed, 0, "the borrower must not close or return an unconfirmed reader to a pool");
    }
}

#[tokio::test]
async fn retiring_either_read_leaves_the_existing_owner_responsible_for_confirmed_shutdown() {
    use std::time::Duration;
    for pause in [1, 2] {
        for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
            let mut reader = FakeRead::new(vec![Ok(fields::all_columns())]).pause_on_query(pause);
            reader.shutdown = shutdown;
            {
                let work = read(&mut reader, 10);
                tokio::pin!(work);
                assert!(tokio::time::timeout(Duration::from_millis(5), &mut work).await.is_err());
            }
            assert_eq!(reader.calls.len(), pause);
            assert_eq!(reader.closed, 0);
            assert_eq!(reader.close().await, shutdown);
            assert_eq!(reader.closed, 1);
        }
    }
}
