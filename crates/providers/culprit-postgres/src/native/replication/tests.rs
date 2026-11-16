use super::project::{ReceiverPresence, Standby, standby_replica};
use super::*;

pub(super) fn native_row(source: &str, id: &str, values: Value) -> Row {
    Row::from_fields([
        ("native_source".into(), Some(source.into())),
        ("native_id".into(), Some(id.into())),
        ("native_values".into(), Some(values.to_string())),
    ])
}

pub(super) fn slot() -> Value {
    serde_json::json!({
        "kind":"slot","name":"consumer","slot_name":"consumer","slot_type":"logical",
        "database_oid":"16384","database":"application","active":true,
        "active_pid":"42","backend_start":"2026-11-13T06:00:00.123456Z",
        "restart_lsn":"0/1234","confirmed_flush_lsn":"0/FFFF",
        "kept":"9007199254740993","safe_wal_size":"0",
        "server_version_num":"170000","read_database_oid":"16384","read_standby":false,
        "observed_at":"2026-11-13T06:00:01.123456Z","full_visibility":true
    })
}

pub(super) fn sender() -> Value {
    serde_json::json!({
        "kind":"sender","name":"replica","active_pid":"42","backend_start":"2026-11-13T06:00:00.123456Z",
        "logical":true,"database":"application","database_oid":"16384","slot_name":"consumer",
        "state":"streaming","sync_state":"sync","sync_priority":"1",
        "current_lsn":"FFFFFFFF/FFFFFFFF","sent_lsn":"FFFFFFFF/FFFFFFFE",
        "write_lsn":"FFFFFFFF/FFFFFFFD","flush_lsn":"FFFFFFFF/FFFFFFFC","replay_lsn":"FFFFFFFF/FFFFFFFB",
        "write_lag":"00:00:00.123456","flush_lag":"00:00:01.123456","replay_lag":"00:00:02.123456",
        "write_lag_s":"0.123456","flush_lag_s":"1.123456","lag_s":"2.123456","behind":"4",
        "reply_time":"2026-11-13T06:00:00.654321Z","column_availability":{"reply_time":true},
        "server_version_num":"170000","read_database_oid":"16384","read_standby":false,
        "observed_at":"2026-11-13T06:00:01.123456Z","full_visibility":true
    })
}

#[test]
fn standby_commit_age_remains_context_even_with_large_backlog_or_intentional_delay() {
    for (behind, age, delay) in
        [("0", "3600", "0"), ("8192", "3600", "0"), ("1073741824", "86400", "0"), ("1073741824", "3640", "3600")]
    {
        let observation = decode(&native_row(
            "pg_recovery",
            "local",
            serde_json::json!({
                "kind":"standby","name":"this server","receiver":"streaming","streaming_configured":true,
                "receive_lsn":"0/FFFFFFFF","replay_lsn":"0/1234","behind":behind,"since_replay":age,"delay_s":delay,
                "last_replay_commit_at":"2026-11-12T06:00:00.123456Z"
            }),
        ))
        .unwrap();
        let replica = project(&observation.fact).unwrap();
        assert_eq!(replica.lag_seconds, None, "commit age is not replay latency at any backlog");
        assert_eq!(replica.lag_bytes, Some(behind.parse().unwrap()));
        let ReplicationFact::Native(sample) = observation.fact else { panic!() };
        assert!(sample.measurements.counters.iter().any(|counter| counter.key == "since_replay"));
        assert_eq!(sample.measurements.properties["last_replay_commit_at"], "2026-11-12T06:00:00.123456Z");
    }
}

#[test]
fn receiver_status_and_streaming_configuration_remain_separate_from_apply_latency() {
    let none = |configured| Standby {
        behind: None,
        receiver: None,
        receiver_presence: ReceiverPresence::Absent,
        streaming_configured: configured,
        delay: None,
    };
    let stopped = standby_replica(none(Some(true)));
    assert_eq!(stopped.condition, ReplicaCondition::NotReceiving);
    assert!(stopped.condition.is_stopped());
    let archive = standby_replica(none(Some(false)));
    assert_eq!((archive.condition, archive.lag_seconds), (ReplicaCondition::RestoringFromArchive, None));
    assert_eq!(standby_replica(none(None)).condition, ReplicaCondition::NotStreaming);
    assert_eq!(
        standby_replica(Standby { receiver: Some("stopped".into()), ..none(Some(true)) }).condition,
        ReplicaCondition::Stopped
    );
    assert_eq!(
        standby_replica(Standby { receiver: Some("streaming".into()), ..none(Some(true)) }).condition,
        ReplicaCondition::Reported
    );
}

#[test]
fn hidden_receiver_status_preserves_positive_pid_and_never_becomes_a_missing_receiver() {
    for (pid, visible, status, expected) in [
        (Some("73"), false, None, ReplicaCondition::Reported),
        (Some("73"), true, None, ReplicaCondition::Reported),
        (None, false, None, ReplicaCondition::Reported),
        (None, true, None, ReplicaCondition::NotReceiving),
        (Some("73"), false, Some("stopped"), ReplicaCondition::Stopped),
        (Some("73"), false, Some("streaming"), ReplicaCondition::Reported),
    ] {
        let row = decode(&native_row(
            "pg_recovery",
            "local",
            serde_json::json!({
                "kind":"standby","name":"this server","receiver":status,"active_pid":pid,
                "full_visibility":visible,"streaming_configured":true,
                "behind":"8192","since_replay":"86400","delay_s":"0"
            }),
        ))
        .unwrap();
        let replica = project(&row.fact).unwrap();
        assert_eq!(replica.condition, expected, "pid={pid:?}, full_visibility={visible}, status={status:?}");
        assert_eq!(
            replica.condition.is_stopped(),
            matches!(expected, ReplicaCondition::Stopped | ReplicaCondition::NotReceiving)
        );
        assert_eq!(replica.lag_bytes, Some(8192.0));
        assert_eq!(replica.lag_seconds, None);
        let ReplicationFact::Native(sample) = &row.fact else { panic!() };
        assert!(sample.session.is_none());
        assert_eq!(sample.unresolved_session_id.as_deref(), pid);
        assert!(sample.measurements.counters.iter().any(|counter| counter.key == "since_replay"));
    }
    let unknown = decode(&native_row(
        "pg_recovery",
        "local",
        serde_json::json!({
            "kind":"standby","name":"this server","streaming_configured":true
        }),
    ))
    .unwrap();
    assert_eq!(project(&unknown.fact).unwrap().condition, ReplicaCondition::Reported);
}

#[test]
fn sender_transport_acknowledgements_keep_exact_positions_intervals_and_native_reply_clock() {
    let observation = decode(&native_row("pg_stat_replication", "42", sender())).unwrap();
    let ReplicationFact::Native(sample) = &observation.fact else { panic!() };
    let set = &sample.measurements;
    for (key, value) in [
        ("current_lsn", "FFFFFFFF/FFFFFFFF"),
        ("sent_lsn", "FFFFFFFF/FFFFFFFE"),
        ("reply_time", "2026-11-13T06:00:00.654321Z"),
        ("write_lag", "00:00:00.123456"),
        ("flush_lag", "00:00:01.123456"),
        ("replay_lag", "00:00:02.123456"),
    ] {
        assert_eq!(set.properties[key], value);
    }
    assert_eq!(set.scope, MeasurementScope::Server);
    assert_eq!(sample.session.as_ref().unwrap().generation, "2026-11-13T06:00:00.123456Z");
    assert_eq!(project(&observation.fact).unwrap().lag_seconds, Some(2.123456));
    assert!(set.counters.iter().all(|counter| counter.reset == culprit_core::ResetMarker::Unknown));
}

#[test]
fn logical_slot_retention_is_exact_server_scoped_and_pid_only_links_stay_unresolved() {
    let mut values = slot();
    let observation = decode(&native_row("pg_replication_slots", "consumer", values.clone())).unwrap();
    let ReplicationFact::Native(sample) = observation.fact else { panic!() };
    assert_eq!(sample.measurements.scope, MeasurementScope::Server);
    assert!(sample.measurements.counters.iter().all(|counter| counter.scope == MeasurementScope::Server));
    assert!(sample.measurements.counters.iter().any(|counter| counter.value.integer() == Some(9_007_199_254_740_993)));
    assert!(
        sample
            .measurements
            .counters
            .iter()
            .any(|counter| counter.key == "safe_wal_size" && counter.value.integer() == Some(0))
    );
    assert_eq!(sample.slot_name.as_deref(), Some("consumer"));
    values["backend_start"] = Value::Null;
    values["safe_wal_size"] = Value::Null;
    let observation = decode(&native_row("pg_replication_slots", "consumer", values)).unwrap();
    let ReplicationFact::Native(sample) = observation.fact else { panic!() };
    assert!(sample.session.is_none());
    assert_eq!(sample.unresolved_session_id.as_deref(), Some("42"));
    assert!(sample.measurements.counters.iter().all(|counter| counter.key != "safe_wal_size"));
}

#[test]
fn receiver_positions_and_clocks_do_not_invent_backend_generation_or_confirm_a_pause_request() {
    let values = serde_json::json!({
        "kind":"standby","name":"this server","active_pid":"73","receiver":"streaming",
        "receive_lsn":"1/FFFFFFF0","replay_lsn":"1/FFFF0000","receive_start_lsn":"1/00000000",
        "receive_start_tli":"5","received_tli":"5","written_lsn":"1/FFFFFFF0","flushed_lsn":"1/FFFFFF00",
        "last_msg_send_time":"2026-11-13T05:59:59.123456Z","last_msg_receipt_time":"2026-11-13T06:00:00.234567Z",
        "latest_end_lsn":"2/10","latest_end_time":"2026-11-13T06:00:00.345678Z",
        "slot_name":"physical_replica","streaming_configured":true,"delay_s":"0","behind":"65520",
        "last_replay_commit_at":"2026-11-13T05:00:00.000001Z","since_replay":"3600",
        "replay_pause_requested":true,"replay_pause_state":"pause requested",
        "replay_pause_access":{"requested":true,"state":true},
        "column_availability":{"receiver_received_lsn":false,"written_lsn":true,"flushed_lsn":true,"replay_pause_state":true},
        "server_version_num":"170000","read_database_oid":"16384","read_standby":true,
        "observed_at":"2026-11-13T06:00:01.123456Z","full_visibility":true
    });
    let row = decode(&native_row("pg_recovery", "local", values)).unwrap();
    let ReplicationFact::Native(sample) = &row.fact else { panic!() };
    assert!(sample.session.is_none());
    assert_eq!(sample.unresolved_session_id.as_deref(), Some("73"));
    for (key, value) in [
        ("flushed_lsn", "1/FFFFFF00"),
        ("latest_end_lsn", "2/10"),
        ("last_msg_receipt_time", "2026-11-13T06:00:00.234567Z"),
        ("replay_pause_state", "pause requested"),
    ] {
        assert_eq!(sample.measurements.properties[key], value);
    }
    assert!(!sample.measurements.properties.contains_key("replay_paused"));
    assert!(!sample.measurements.properties.contains_key("receiver_received_lsn"));
    assert_eq!(project(&row.fact).unwrap().lag_seconds, None);
    assert!(sample.measurements.properties.len() <= MAX_NATIVE_QUALIFIERS);
}

#[test]
fn synchronized_slots_and_unknown_sync_status_do_not_become_abandoned_consumers() {
    for (synced, columns, ordinary) in [
        (serde_json::json!(true), serde_json::json!({"synced":true}), false),
        (Value::Null, serde_json::json!({"synced":true}), false),
        (Value::Null, Value::Null, false),
        (serde_json::json!(false), serde_json::json!({"synced":true}), true),
        (Value::Null, serde_json::json!({"synced":false}), true),
    ] {
        let mut values = slot();
        values["active"] = serde_json::json!(false);
        values["active_pid"] = Value::Null;
        values["backend_start"] = Value::Null;
        values["synced"] = synced;
        values["column_availability"] = columns;
        values["inactive_since"] = serde_json::json!("2026-11-12T06:00:00.123456Z");
        values["inactive_s"] = serde_json::json!("86400");
        let row = decode(&native_row("pg_replication_slots", "consumer", values)).unwrap();
        let replica = project(&row.fact).unwrap();
        assert_eq!(
            replica.condition,
            if ordinary { ReplicaCondition::SlotInactive } else { ReplicaCondition::Reported }
        );
        assert_eq!(replica.inactive_seconds, ordinary.then_some(86400.0));
        let ReplicationFact::Native(sample) = row.fact else { panic!() };
        assert_eq!(sample.measurements.properties["inactive_since"], "2026-11-12T06:00:00.123456Z");
        assert!(sample.measurements.counters.iter().any(|counter| counter.key == "inactive_s"));
    }
}

#[test]
fn complete_native_slot_and_sender_layouts_fit_the_existing_property_envelope() {
    let mut values = slot();
    for (key, value) in [
        ("xmin", "100"),
        ("catalog_xmin", "101"),
        ("plugin", "pgoutput"),
        ("wal_status", "reserved"),
        ("two_phase_at", "0/AAAA"),
    ] {
        values[key] = serde_json::json!(value);
    }
    for (key, value) in
        [("temporary", false), ("two_phase", true), ("conflicting", false), ("failover", true), ("synced", false)]
    {
        values[key] = serde_json::json!(value);
    }
    values["column_availability"] = serde_json::json!({"wal_status":true,"safe_wal_size":true,"two_phase":true,"two_phase_at":true,"conflicting":true,"inactive_since":true,"invalidation_reason":true,"failover":true,"synced":true});
    values["server_version_num"] = serde_json::json!("180000");
    for inactive in [false, true] {
        let mut values = values.clone();
        if inactive {
            values["active"] = serde_json::json!(false);
            values["active_pid"] = Value::Null;
            values["backend_start"] = Value::Null;
            values["inactive_since"] = serde_json::json!("2026-11-12T06:00:00.123456Z");
            values["inactive_s"] = serde_json::json!("86400");
            values["wal_status"] = serde_json::json!("lost");
            values["invalidation_reason"] = serde_json::json!("wal_removed");
            values["safe_wal_size"] = Value::Null;
        }
        let row = decode(&native_row("pg_replication_slots", "consumer", values)).unwrap();
        let ReplicationFact::Native(sample) = row.fact else { panic!() };
        assert!(sample.measurements.properties.len() <= MAX_NATIVE_QUALIFIERS);
    }
    let mut values = sender();
    values["backend_xmin"] = serde_json::json!("100");
    assert!(decode(&native_row("pg_stat_replication", "42", values)).is_ok());
}

#[test]
fn unavailable_optional_slot_columns_differ_from_native_null_and_false_without_retaining_secrets() {
    let mut values = slot();
    values["failover"] = Value::Null;
    values["column_availability"] = serde_json::json!({"failover":true,"synced":false});
    values["conninfo"] = serde_json::json!("password=private-replication-secret");
    let observation = decode(&native_row("pg_replication_slots", "consumer", values.clone())).unwrap();
    let ReplicationFact::Native(sample) = observation.fact else { panic!() };
    assert!(!sample.measurements.properties.contains_key("failover"));
    assert_eq!(sample.measurements.properties["column_availability"], "{\"failover\":true,\"synced\":false}");
    assert!(!serde_json::to_string(&sample).unwrap().contains("private-replication-secret"));
    values["failover"] = serde_json::json!(false);
    let observation = decode(&native_row("pg_replication_slots", "consumer", values)).unwrap();
    let ReplicationFact::Native(sample) = observation.fact else { panic!() };
    assert_eq!(sample.measurements.properties["failover"], "false");
}

#[test]
fn negative_distances_remain_context_and_unknown_active_or_logical_flags_are_not_false() {
    let mut values = slot();
    values["kept"] = serde_json::json!("-1");
    values["active"] = Value::Null;
    values["slot_type"] = Value::Null;
    let observation = decode(&native_row("pg_replication_slots", "consumer", values)).unwrap();
    let ReplicationFact::Native(sample) = &observation.fact else { panic!() };
    assert_eq!(sample.measurements.properties["kept_native"], "-1");
    assert!(sample.measurements.counters.iter().all(|counter| counter.key != "kept"));
    let replica = project(&observation.fact).unwrap();
    assert_eq!(replica.condition, ReplicaCondition::Reported);
    assert_eq!(replica.logical, None);
    let mut values = sender();
    values["logical"] = Value::Null;
    assert_eq!(project(&decode(&native_row("pg_stat_replication", "42", values)).unwrap().fact).unwrap().logical, None);
}

#[test]
fn native_sql_uses_supported_receiver_columns_and_only_bounded_existing_views() {
    for version in [100_000, 120_000, 130_000, 140_000, 170_000, 180_000] {
        let sql = query(version, true, "16384", 17).unwrap();
        assert!(sql.ends_with("LIMIT 17"));
        assert_eq!(sql.contains("w.written_lsn"), version >= 130_000);
        assert_eq!(sql.contains("w.received_lsn"), version < 130_000);
        assert_eq!(sql.contains("pg_get_wal_replay_pause_state()"), version >= 140_000);
        for forbidden in [
            "w.conninfo",
            "conninfo::text",
            "to_jsonb(w)",
            "SELECT * FROM pg_catalog.pg_stat_wal_receiver",
            "pg_stat_subscription",
            "pg_stat_replication_slots",
            "pg_logical_slot_get_changes",
            "pg_wal_replay_pause(",
            "pg_wal_replay_resume(",
        ] {
            assert!(!sql.contains(forbidden), "{version}: {forbidden}");
        }
        assert!(sql.contains("'streaming_configured',(SELECT setting<>''"), "only a boolean leaves the server");
        assert!(sql.contains("s.datoid IS NULL OR s.datoid=16384"));
    }
    assert!(query(90_600, false, "16384", 17).is_err());
    assert!(query(170_000, false, "16384; SELECT", 17).is_err());
    let sql = query(170_000, false, "16384", 17).unwrap();
    for name in [
        "reply_time",
        "temporary",
        "plugin",
        "two_phase",
        "two_phase_at",
        "conflicting",
        "inactive_since",
        "invalidation_reason",
        "failover",
        "synced",
        "column_availability",
    ] {
        assert!(sql.contains(name), "{name}");
    }
    assert!(!sql.contains("coalesce(s.slot_type='logical',false)"));
    assert!(!sql.contains("s.inactive_since"), "absent native columns are tested as row keys, not bound eagerly");
}

#[test]
fn malformed_native_clock_position_counter_actor_or_source_kind_is_rejected() {
    for (key, value) in [
        ("backend_start", "not-a-time"),
        ("restart_lsn", "100000000/0"),
        ("restart_lsn", "not-an-lsn"),
        ("kept", "01"),
        ("kept", "-broken"),
        ("kept", "18446744073709551616"),
        ("active_pid", "0"),
        ("active_pid", "2147483648"),
        ("kind", "sender"),
    ] {
        let mut values = slot();
        values[key] = serde_json::json!(value);
        assert!(decode(&native_row("pg_replication_slots", "consumer", values)).is_err(), "{key}={value}");
    }
}
