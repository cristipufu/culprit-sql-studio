use super::*;
use crate::read::test_support::Read;
use mysql_async::Value;

fn mysql(channel: &str) -> Rec {
    Rec::from_values(vec![
        ("Channel_Name", channel.into()),
        ("Source_Host", "source.example.invalid".into()),
        ("Replica_IO_Running", "Connecting".into()),
        ("Replica_SQL_Running", "Yes".into()),
        ("Seconds_Behind_Source", Value::NULL),
        ("SQL_Delay", Value::UInt(0)),
        ("Relay_Log_Space", Value::UInt(42)),
        ("Last_IO_Errno", Value::UInt(1045)),
        ("Last_SQL_Errno", Value::UInt(0)),
    ])
}

fn maria(channel: &str) -> Rec {
    Rec::from_values(vec![
        ("Connection_name", channel.into()),
        ("Master_Host", "primary.example.invalid".into()),
        ("Slave_IO_Running", "Yes".into()),
        ("Slave_SQL_Running", "No".into()),
        ("Seconds_Behind_Master", Value::UInt(7)),
        ("SQL_Delay", Value::UInt(0)),
        ("Relay_Log_Space", Value::UInt(42)),
        ("Last_IO_Errno", Value::UInt(0)),
        ("Last_SQL_Errno", Value::UInt(1062)),
    ])
}

fn sample(row: &Rec) -> NativeReplicationSample {
    let TelemetryRecord::Replication(ReplicationObservation { fact: ReplicationFact::Native(sample), .. }) =
        decode(row).unwrap()
    else {
        panic!("expected native channel");
    };
    *sample
}

fn counter<'a>(sample: &'a NativeReplicationSample, key: &str) -> &'a NativeCounterSample {
    sample.measurements.counters.iter().find(|counter| counter.key == key).unwrap()
}

#[test]
fn renamed_status_commands_are_selected_from_the_native_product_version() {
    for (flavor, version, expected) in [
        (Flavor::Mysql, "5.7.44", "SHOW SLAVE STATUS"),
        (Flavor::Mysql, "8.0.21", "SHOW SLAVE STATUS"),
        (Flavor::Mysql, "8.0.22", "SHOW REPLICA STATUS"),
        (Flavor::Mysql, "8.4.11", "SHOW REPLICA STATUS"),
        (Flavor::Mariadb, "10.5.0-MariaDB", "SHOW ALL SLAVES STATUS"),
        (Flavor::Mariadb, "10.5.1-MariaDB", "SHOW ALL REPLICAS STATUS"),
        (Flavor::Mariadb, "5.5.5-10.11.9-MariaDB", "SHOW ALL REPLICAS STATUS"),
    ] {
        assert_eq!(query(flavor, version), expected, "{version}");
    }
}

#[test]
fn receiver_applier_positions_delay_skip_and_error_codes_remain_exact_native_points() {
    let point = sample(&mysql("").with_fields(&[
        ("source_log_file", "bin.000002".into()),
        ("read_source_log_pos", "9007199254740993".into()),
        ("relay_source_log_file", "bin.000001".into()),
        ("exec_source_log_pos", Value::UInt(u64::MAX)),
        ("relay_log_file", "relay.000004".into()),
        ("relay_log_pos", Value::UInt(4)),
        ("sql_remaining_delay", Value::UInt(0)),
        ("skip_counter", Value::UInt(0)),
        ("auto_position", Value::UInt(1)),
        ("last_io_error_timestamp", "261115 01:02:03".into()),
        ("last_sql_error_timestamp", "".into()),
    ]));
    assert!(point.is_valid());
    assert_eq!(point.measurements.scope, MeasurementScope::Server);
    assert_eq!(
        counter(&point, "received-source-log-position").value,
        MeasurementValue::Integer("9007199254740993".into())
    );
    assert_eq!(counter(&point, "executed-source-log-position").value, MeasurementValue::Integer(u64::MAX.to_string()));
    assert_eq!(counter(&point, "remaining-delay").value.integer(), Some(0));
    assert_eq!(counter(&point, "skip-counter").value.integer(), Some(0));
    assert_eq!(counter(&point, "configured-delay").value.integer(), Some(0));
    assert_eq!(counter(&point, "io-error-code").value.integer(), Some(1045));
    assert_eq!(counter(&point, "sql-error-code").value.integer(), Some(0));
    assert!(point.measurements.counters.iter().all(|counter| counter.key != "lag-seconds"));
    assert_eq!(point.measurements.properties["channel"], "");
    assert_eq!(point.measurements.properties["io_error_at_without_offset"], "261115 01:02:03");
    assert!(!point.measurements.properties.contains_key("sql_error_at_without_offset"));
    assert_eq!(counter(&point, "received-source-log-position").native_name, "read_source_log_pos");
    assert_eq!(counter(&point, "received-source-log-position").unit, CounterUnit::Bytes);
}

#[test]
fn native_invalid_gtid_only_positions_are_not_repaired_or_subtracted_into_backlog() {
    let point = sample(&mysql("gtid").with_fields(&[
        ("source_log_file", "INVALID".into()),
        ("relay_source_log_file", "INVALID".into()),
        ("read_source_log_pos", Value::UInt(0)),
        ("exec_source_log_pos", Value::UInt(0)),
        ("auto_position", Value::UInt(1)),
    ]));
    assert_eq!(point.measurements.properties["source_log_file"], "INVALID");
    assert_eq!(counter(&point, "received-source-log-position").value.integer(), Some(0));
    assert_eq!(counter(&point, "executed-source-log-position").value.integer(), Some(0));
    assert!(point.measurements.counters.iter().all(|counter| !counter.key.contains("backlog")));
}

#[test]
fn maria_parallel_context_retains_totals_without_claiming_counter_reset_or_current_failure() {
    let point = sample(&maria("orders").with_fields(&[
        ("parallel_mode", "optimistic".into()),
        ("using_gtid", "Slave_Pos".into()),
        ("slave_sql_state", "Updating".into()),
        ("slave_sql_running_state", "Waiting for work from SQL thread".into()),
        ("slave_ddl_groups", Value::UInt(9_007_199_254_740_993)),
        ("slave_non_transactional_groups", Value::UInt(0)),
        ("slave_transactional_groups", Value::UInt(45)),
        ("retried_transactions", Value::UInt(2)),
        ("executed_log_entries", Value::UInt(51)),
        ("slave_received_heartbeats", Value::UInt(8)),
        ("master_last_event_time", "2026-11-15 08:00:00".into()),
        ("slave_last_event_time", "2026-11-15 07:59:58".into()),
    ]));
    assert_eq!(point.measurements.properties["parallel_mode"], "optimistic");
    assert_eq!(point.measurements.properties["sql_state"], "Updating");
    assert_eq!(point.measurements.properties["sql_running_state"], "Waiting for work from SQL thread");
    assert_eq!(point.measurements.properties["source_last_event_at_without_offset"], "2026-11-15 08:00:00");
    for key in [
        "ddl-groups",
        "non-transactional-groups",
        "transactional-groups",
        "retried-transactions",
        "executed-log-entries",
        "received-heartbeats",
    ] {
        let counter = counter(&point, key);
        assert_eq!(counter.nature, CounterNature::Cumulative);
        assert_eq!(counter.reset, culprit_core::ResetMarker::Unknown);
    }
    assert_eq!(counter(&point, "ddl-groups").value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(counter(&point, "non-transactional-groups").value.integer(), Some(0));
}

#[test]
fn native_column_absence_null_and_zero_never_become_each_other() {
    let omitted = sample(&mysql("a"));
    let null = sample(&mysql("a").with_fields(&[("sql_remaining_delay", Value::NULL)]));
    let zero = sample(&mysql("a").with_fields(&[("sql_remaining_delay", Value::UInt(0))]));
    for point in [&omitted, &null] {
        assert!(point.measurements.counters.iter().all(|counter| counter.key != "remaining-delay"));
        assert!(!point.measurements.properties.contains_key("parallel_mode"));
    }
    assert_eq!(counter(&zero, "remaining-delay").value.integer(), Some(0));
    for value in [Value::Int(-1), Value::Double(1.0), "1.5".into(), "18446744073709551616".into()] {
        assert!(decode(&mysql("a").with_fields(&[("read_source_log_pos", value)])).is_err());
    }
    assert!(decode(&mysql("a").with_fields(&[("channel_name", Value::NULL)])).is_err());
    assert!(decode(&mysql("a").with_fields(&[("last_io_error_timestamp", "private query text".into())])).is_err());
}

#[test]
fn bounded_filter_context_is_exact_or_explicitly_withheld_and_secrets_never_enter_the_fact() {
    let point = sample(&mysql("private_channel").with_fields(&[
        ("replicate_do_db", "orders,archive".into()),
        ("replicate_ignore_db", "".into()),
        ("replicate_rewrite_db", "(source_db,local_db)".into()),
        ("replicate_wild_do_table", "x".repeat(2_049).into()),
        ("source_user", "PRIVATE_LOGIN_CANARY".into()),
        ("source_password", "PRIVATE_PASSWORD_CANARY".into()),
        ("source_ssl_key", "PRIVATE_KEY_PATH_CANARY".into()),
        ("last_io_error", "PRIVATE_ERROR_SQL_CANARY".into()),
        ("last_sql_error", Value::Bytes(vec![0xff; 8])),
        ("retrieved_gtid_set", "PRIVATE_RETRIEVED_GTID_CANARY".repeat(4_096).into()),
        ("executed_gtid_set", "PRIVATE_EXECUTED_GTID_CANARY".repeat(4_096).into()),
    ]));
    assert_eq!(point.measurements.properties["replicate_do_db"], "orders,archive");
    assert_eq!(point.measurements.properties["replicate_ignore_db"], "");
    assert_eq!(point.measurements.properties["withheld_fields"], "replicate_wild_do_table");
    assert!(!point.measurements.properties.contains_key("replicate_wild_do_table"));
    let json = serde_json::to_string(&point).unwrap();
    for private in [
        "PRIVATE_LOGIN_CANARY",
        "PRIVATE_PASSWORD_CANARY",
        "PRIVATE_KEY_PATH_CANARY",
        "PRIVATE_ERROR_SQL_CANARY",
        "PRIVATE_RETRIEVED_GTID_CANARY",
        "PRIVATE_EXECUTED_GTID_CANARY",
    ] {
        assert!(!json.contains(private), "{private}");
    }
}

#[tokio::test]
async fn both_native_products_use_one_bounded_show_for_every_channel_without_optional_followup() {
    for (flavor, version, rows, command) in [
        (Flavor::Mysql, "8.4.11", vec![mysql(""), mysql("orders")], "SHOW REPLICA STATUS"),
        (Flavor::Mariadb, "10.11.9-MariaDB", vec![maria(""), maria("orders")], "SHOW ALL REPLICAS STATUS"),
    ] {
        let mut reader = Read::with(vec![Ok(rows)]).with_query_budget(1, 0);
        let points = read(&mut reader, flavor, version).await.unwrap();
        assert_eq!(points.len(), 2);
        assert_ne!(points[0].fact.key(), points[1].fact.key());
        assert_eq!(reader.queries.len(), 1);
        assert_eq!(reader.queries[0], (command.into(), vec![], culprit_core::limits::MAX_RESULT_ROWS));
        assert!(reader.replies.is_empty());
        assert_eq!(reader.closed, 0, "the borrowed outer reader retains native lifecycle ownership");
    }
}

#[tokio::test]
async fn failure_empty_and_duplicate_channels_preserve_the_native_read_result_without_retry() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::RowLimit,
    ] {
        let mut reader = Read::with(vec![Err(stop.error())]);
        assert_eq!(
            read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(stop))
        );
        assert_eq!(reader.queries.len(), 1);
    }
    let mut reader = Read::with(vec![Ok(vec![])]);
    assert!(read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap().is_empty());
    let mut reader = Read::with(vec![Ok(vec![mysql("same"), mysql("same")])]);
    assert!(read(&mut reader, Flavor::Mysql, "8.4.11").await.is_err());
    assert_eq!(reader.queries.len(), 1);
}
