use super::test_support::*;
use super::*;
use crate::value::Val;

#[tokio::test]
async fn snapshot_and_export_keep_the_same_native_values_precision_and_scopes() {
    let raw = row("2020-01-01T06:00:15.1533333Z");
    let mut snapshot_reader = FakeRead::new(vec![Ok(vec![raw.clone()])]);
    let snapshot = snapshot_from(&mut snapshot_reader).await.unwrap();
    let displayed = snapshot[0].snapshot();
    let mut export_reader = FakeRead::new(vec![Ok(vec![raw])]);
    let exported = windowed(&mut export_reader, &window(), None, 10)
        .await
        .map_err(ReadError::into_error)
        .unwrap()
        .remove(0)
        .exported();
    use culprit_core::MeasurementScope::{Database, Server};
    let metric =
        |key: &str, scope| exported.metrics.iter().find(|metric| metric.key == key && metric.scope == scope).unwrap();
    assert!(exported.is_valid());
    assert_eq!(displayed.data_io_accounting, exported.data_io_accounting);
    assert_eq!(displayed.data_io_accounting, DataIoAccounting::DataFiles);
    assert_eq!(displayed.at, exported.at);
    assert_eq!(displayed.at, "2020-01-01T06:00:15.153333300Z");
    assert_eq!(displayed.cpu_pct, Some(0.0));
    assert_eq!(displayed.cpu_pct, metric("cpu", Database).value);
    assert_eq!(displayed.data_io_pct, metric("data-io", Database).value);
    assert_eq!(displayed.log_write_pct, metric("log-write", Database).value);
    assert_eq!(displayed.memory_pct, None);
    assert_eq!(displayed.memory_pct, metric("memory", Database).value);
    assert_eq!(displayed.workers_pct, metric("workers", Database).value);
    assert_eq!(metric("workers", Database).statistic, ResourceStatistic::Maximum);
    assert_eq!(metric("cpu", Server).period_ms, Some(60_000));
    assert_eq!(metric("cpu", Server).value, Some(70.0));
    assert_eq!(metric("xtp-storage", Database).limit_basis, ResourceLimitBasis::PoolLimit);
    assert_eq!(metric("cpu-limit", Database).unit, ResourceUnit::Cores);
    assert_eq!(metric("dtu-limit", Database).value, None);
    assert_eq!(snapshot_reader.closed, 1);
    assert_eq!(snapshot_reader.calls.len(), 1);
    assert_eq!(export_reader.calls.len(), 1);
    assert_eq!(snapshot_reader.calls[0].sql, sql::DATABASE.retained());
    assert_eq!(export_reader.calls[0].sql, sql::DATABASE.windowed());
    assert_eq!(export_reader.calls[0].limit, 11);
    assert!(matches!(&export_reader.calls[0].parameters[0], Parameter::Integer(11)));
}
#[tokio::test]
async fn unknown_metrics_do_not_become_zero_pressure_and_shared_cpu_does_not_replace_database_cpu() {
    let first = replace(&row("2020-01-01T06:00:15Z"), &[("avg_memory_usage_percent", Val::Null)]);
    let second = replace(
        &row("2020-01-01T06:00:30Z"),
        &[("avg_data_io_percent", Val::Null), ("avg_memory_usage_percent", Val::Null)],
    );
    let mut io = FakeRead::new(vec![Ok(vec![first, second])]);
    let samples = retained(&mut io).await.map_err(ReadError::into_error).unwrap();
    let summary = Summary::from_samples(&samples);
    assert_eq!(summary.data_io, Some(20.0));
    assert_eq!(summary.data_io_peak, Some(20.0));
    assert_eq!(summary.memory, None);
    let signals = crate::pressure::resource_signals(&summary);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].key, culprit_core::signal_id::DATA_IO);
    assert_eq!(samples[0].value(Metric::Cpu), Some(0.0));
    assert_eq!(samples[0].value(Metric::InstanceCpu), Some(70.0));
    let empty = crate::pressure::resource_signals(&Summary::default());
    assert!(empty.is_empty());
}
#[tokio::test]
async fn snapshot_summaries_aggregate_only_known_native_samples() {
    let mut io = FakeRead::new(vec![Ok(vec![
        row("2020-01-01T06:00:15Z"),
        replace(
            &row("2020-01-01T06:00:30Z"),
            &[("avg_data_io_percent", Val::Float(40.0)), ("avg_memory_usage_percent", Val::Float(75.0))],
        ),
    ])]);
    let samples = retained(&mut io).await.map_err(ReadError::into_error).unwrap();
    let summary = Summary::from_samples(&samples);
    assert_eq!(summary.data_io, Some(30.0));
    assert_eq!(summary.data_io_peak, Some(40.0));
    assert_eq!(summary.memory, Some(75.0));
    assert_eq!(io.calls.len(), 1);
}
#[tokio::test]
async fn malformed_pages_duplicates_and_out_of_window_rows_never_publish_a_partial_page() {
    for rows in [
        vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:15Z")],
        vec![row("2020-01-01T06:01:00Z")],
        vec![row("malformed")],
        vec![
            row("2020-01-01T06:00:15Z"),
            replace(&row("2020-01-01T06:00:30Z"), &[("avg_cpu_percent", Val::Float(f64::NAN))]),
        ],
    ] {
        let mut io = FakeRead::new(vec![Ok(rows)]);
        assert!(windowed(&mut io, &window(), None, 1).await.is_err());
    }
    let mut io = FakeRead::new(vec![]);
    assert!(windowed(&mut io, &window(), Some("foreign"), 1).await.is_err());
    assert!(windowed(&mut io, &window(), Some("2020-01-01T05:00:00Z"), 1).await.is_err());
    assert!(io.calls.is_empty());
}
#[tokio::test]
async fn native_secondary_samples_keep_their_replica_identity() {
    let mut io = FakeRead::new(vec![Ok(vec![replace(&row("2020-01-01T06:00:15Z"), &[("replica_role", Val::Int(2))])])]);
    let sample = retained(&mut io).await.map_err(ReadError::into_error).unwrap().remove(0);
    assert_eq!(sample.exported().replica_id.as_deref(), Some("2"));
    assert_eq!(sample.snapshot().cpu_pct, Some(0.0));
}
#[tokio::test]
async fn failures_and_cancellation_close_the_snapshot_owner_before_returning() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::ByteLimit] {
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let error = snapshot_from(&mut io).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(io.closed, 1);
    }
    let mut io = FakeRead::new(vec![Ok(vec![row("2020-01-01T06:00:15Z")])]);
    io.shutdown = SourceShutdown::Unconfirmed;
    let error = snapshot_from(&mut io).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(io.closed, 1);
}
#[tokio::test]
async fn latest_samples_keep_native_zero_unknown_and_future_clock_values() {
    let mut io = FakeRead::new(vec![
        Ok(vec![latest_row("2020-01-01T06:01:45Z")]),
        Ok(vec![latest_row("2020-01-01T06:02:00.0000001Z")]),
        Ok(vec![]),
    ]);
    let latest = super::latest(&mut io).await.map_err(ReadError::into_error).unwrap().unwrap();
    assert_eq!(latest.age_seconds(), Some(15));
    assert_eq!(latest.sample.value(Metric::Cpu), Some(0.0));
    let latest = super::latest(&mut io).await.map_err(ReadError::into_error).unwrap().unwrap();
    assert_eq!(latest.age_seconds(), None);
    assert!(super::latest(&mut io).await.map_err(ReadError::into_error).unwrap().is_none());
}

#[tokio::test]
async fn retained_row_limit_and_non_numeric_values_fail_without_publishing_snapshot_samples() {
    let mut io = FakeRead::new(vec![Ok(vec![row("2020-01-01T06:00:15Z"); MAX_RETAINED_SAMPLES as usize + 1])]);
    let error = snapshot_from(&mut io).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(io.closed, 1);
    let mut io = FakeRead::new(vec![Ok(vec![replace(
        &row("2020-01-01T06:00:15Z"),
        &[("avg_cpu_percent", Val::Text("0".into()))],
    )])]);
    assert!(snapshot_from(&mut io).await.is_err());
    assert_eq!(io.closed, 1);
}

#[tokio::test]
async fn native_deployment_basis_survives_scan_and_retained_projection_without_another_dispatch() {
    for (edition, database_edition, replica, expected) in [
        (Val::Int(5), Val::Text("Hyperscale".into()), Val::Null, DataIoAccounting::ComputeLocalSsd),
        (
            Val::Int(5),
            Val::Null,
            Val::Text("91487244-730d-4b83-8e49-d482241c7288".into()),
            DataIoAccounting::ComputeLocalSsd,
        ),
        (Val::Int(5), Val::Text("General Purpose".into()), Val::Null, DataIoAccounting::DataFiles),
        (Val::Int(5), Val::Null, Val::Null, DataIoAccounting::Unknown),
        (Val::Int(5), Val::Text("Future Tier".into()), Val::Null, DataIoAccounting::Unknown),
        (Val::Int(6), Val::Text("Hyperscale".into()), Val::Null, DataIoAccounting::Unknown),
        (Val::Null, Val::Null, Val::Null, DataIoAccounting::Unknown),
    ] {
        let raw = replace(
            &row("2020-01-01T06:00:15Z"),
            &[("engine_edition", edition), ("database_edition", database_edition), ("replica_id", replica)],
        );
        let mut io = FakeRead::new(vec![Ok(vec![raw])]);
        let sample = snapshot_from(&mut io).await.unwrap().remove(0);
        assert_eq!(sample.snapshot().data_io_accounting, expected);
        assert_eq!(sample.exported().data_io_accounting, expected);
        assert_eq!(sample.value(Metric::DataIo), Some(20.0));
        assert_eq!(io.calls.len(), 1);
        assert_eq!(io.closed, 1);
        assert!(io.calls[0].sql.contains("DATABASEPROPERTYEX(DB_NAME(),'Edition')"));
        assert!(io.calls[0].sql.contains("DATABASEPROPERTYEX(DB_NAME(),'ReplicaID')"));
    }
}

#[tokio::test]
async fn incompatible_accounting_populations_keep_raw_samples_without_a_pooled_pressure_signal() {
    let local = replace(&row("2020-01-01T06:00:15Z"), &[("database_edition", Val::Text("Hyperscale".into()))]);
    let conventional = row("2020-01-01T06:00:30Z");
    let mut io = FakeRead::new(vec![Ok(vec![local, conventional])]);
    let samples = retained(&mut io).await.unwrap();
    assert_eq!(samples.len(), 2);
    let summary = Summary::from_samples(&samples);
    assert_eq!(summary.data_io, None);
    assert_eq!(summary.data_io_peak, None);
    assert!(crate::pressure::resource_signals(&summary).is_empty());
}

#[test]
fn resource_admission_template_remains_unchanged_while_history_retains_its_native_profile() {
    assert_eq!(
        culprit_core::telemetry::digest(sql::DATABASE.latest().as_bytes()),
        "f8320d0e63befe872bbd529844e2bab1e7653e6ec3dfbf3958a799658e8db429"
    );
    assert!(!sql::DATABASE.latest().contains("DATABASEPROPERTYEX"));
    assert!(sql::DATABASE.retained().contains("DATABASEPROPERTYEX(DB_NAME(),'Edition')"));
    assert!(sql::DATABASE.windowed().contains("DATABASEPROPERTYEX(DB_NAME(),'ReplicaID')"));
    assert!(!sql::POOL.windowed().contains("DATABASEPROPERTYEX"));
}

#[tokio::test]
async fn unchanged_admission_projection_needs_no_unrequested_profile_and_cannot_inherit_one() {
    let native = latest_row("2020-01-01T06:01:45Z");
    let fields: Vec<_> = native
        .columns()
        .filter(|(name, _)| !["engine_edition", "database_edition", "replica_id"].contains(name))
        .map(|(name, value)| (name, value.clone()))
        .collect();
    let mut io = FakeRead::new(vec![Ok(vec![crate::value::Rec::of(&fields)])]);
    let latest = latest(&mut io).await.unwrap().unwrap();
    assert_eq!(latest.sample.data_io_accounting, DataIoAccounting::Unknown);
    assert_eq!(latest.sample.value(Metric::DataIo), Some(20.0));
    assert_eq!(io.calls.len(), 1);
    assert!(!io.calls[0].sql.contains("DATABASEPROPERTYEX"));
}
