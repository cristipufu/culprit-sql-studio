//! Source-owned replication points for the common codec, warehouse and SDK paths. No provider or database I/O.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode, MeasurementScope};

#[derive(Clone, Copy, Debug)]
pub enum ReplicationCase {
    PostgresSender,
    PostgresStandbyPaused,
    PostgresStandbyRequested,
    PostgresSlots,
    PostgresLegacySlot,
    MySqlChannel,
    MariaDbChannel,
    MssqlAvailabilityGroup,
    MssqlGeoLink,
    Partial,
    Empty,
    Unavailable,
    NotApplicable,
    UnqualifiedOwner,
}

impl ReplicationCase {
    pub const ALL: &[Self] = &[
        Self::PostgresSender,
        Self::PostgresStandbyPaused,
        Self::PostgresStandbyRequested,
        Self::PostgresSlots,
        Self::PostgresLegacySlot,
        Self::MySqlChannel,
        Self::MariaDbChannel,
        Self::MssqlAvailabilityGroup,
        Self::MssqlGeoLink,
        Self::Partial,
        Self::Empty,
        Self::Unavailable,
        Self::NotApplicable,
        Self::UnqualifiedOwner,
    ];
}

pub fn replication_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    ReplicationCase::ALL.iter().copied().map(replication_evidence).collect()
}

pub fn replication_evidence(case: ReplicationCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    use ReplicationCase::*;
    let (engine, rows, source_names) = match case {
        PostgresSender | Partial | UnqualifiedOwner => {
            (Engine::Postgres, senders(), vec!["pg_stat_replication", "pg_replication_slots"])
        }
        PostgresStandbyPaused | PostgresStandbyRequested => (
            Engine::Postgres,
            vec![standby(matches!(case, PostgresStandbyPaused))],
            vec!["pg_recovery", "pg_replication_slots"],
        ),
        PostgresSlots => (Engine::Postgres, slots(), vec!["pg_stat_replication", "pg_replication_slots"]),
        PostgresLegacySlot => {
            (Engine::Postgres, vec![legacy_slot()], vec!["pg_stat_replication", "pg_replication_slots"])
        }
        MySqlChannel | MariaDbChannel => {
            (Engine::Mysql, channels(matches!(case, MariaDbChannel)), vec!["replication-channels"])
        }
        MssqlAvailabilityGroup => {
            (Engine::Mssql, availability_group(), vec!["availability-group-state", "geo-replication-links"])
        }
        MssqlGeoLink => (Engine::Mssql, geo_links(), vec!["availability-group-state", "geo-replication-links"]),
        Empty | Unavailable | NotApplicable => {
            (Engine::Postgres, vec![], vec!["pg_stat_replication", "pg_replication_slots"])
        }
    };
    let sources = source_names
        .into_iter()
        .map(|source| {
            let count = rows.iter().filter(|row| row.source == source).count() as u32;
            match case {
                Unavailable => StateSourceCoverage::unavailable(source.into(), ErrorCode::PermissionDenied, None),
                NotApplicable => StateSourceCoverage::not_applicable(source.into()),
                Partial if source == "pg_replication_slots" => {
                    StateSourceCoverage::unavailable(source.into(), ErrorCode::PermissionDenied, None)
                }
                Partial => {
                    let mut coverage = StateSourceCoverage::observed(source.into(), count, false);
                    coverage.stopped = Some(CollectionStop::RowLimit);
                    coverage
                }
                UnqualifiedOwner => StateSourceCoverage::observed(source.into(), count, false),
                MssqlAvailabilityGroup | MssqlGeoLink if count == 0 => {
                    StateSourceCoverage::not_applicable(source.into())
                }
                _ => StateSourceCoverage::observed(source.into(), count, true),
            }
        })
        .collect();
    let source = CollectionDescription {
        target: DatabaseIdentity::server(
            engine,
            "app",
            "replication.fixture.invalid",
            if engine == Engine::Postgres {
                5432
            } else if engine == Engine::Mysql {
                3306
            } else {
                1433
            },
            None,
        )
        .into(),
        partition: if matches!(case, UnqualifiedOwner) {
            SourcePartition::InstanceLocal { member: None, epoch: None }
        } else {
            SourcePartition::Target
        },
        source: match engine {
            Engine::Postgres => "postgres-replication",
            Engine::Mysql => "mysql-replication",
            _ => "mssql-replication",
        }
        .into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Replication,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: rows.len() as u32,
            sources,
        }),
    };
    assert!(source.partition_is_consistent(), "{case:?}");
    let input = ReplicationSnapshot::from_source(&source, rows.clone()).unwrap();
    assert!(input.is_valid(), "{case:?}");
    (source, rows.into_iter().map(TelemetryRecord::Replication).collect())
}

pub fn assert_replication_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_operational_capture(capture, source, records);
    assert_eq!(capture.families(), &[crate::analytical::evidence::EvidenceFamily::ReplicationSamples]);
    let points = capture
        .records()
        .iter()
        .map(|record| {
            let TelemetryRecord::Replication(row) = &record.row().record else { panic!("native replication point") };
            row.clone()
        })
        .collect();
    let input = ReplicationSnapshot::from_source(source, points).unwrap();
    assert!(input.is_valid());
    let context = input.context.native().unwrap();
    assert_eq!(context.target, source.target);
    assert_eq!(context.partition, source.partition);
    assert_eq!(input.sources, source.observation.snapshot().unwrap().sources);
}

fn native(
    source: &str,
    id: &str,
    properties: &[(&str, &str)],
    counters: &[(&str, u64, CounterUnit)],
) -> ReplicationObservation {
    let measurements = NativeCounterSet {
        identity: NativeIdentity::new(source).qualified("native_id", id),
        scope: MeasurementScope::Server,
        properties: properties.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect(),
        counters: counters
            .iter()
            .map(|(key, value, unit)| {
                NativeCounterSample::gauge(key, *value, *unit, MeasurementScope::Server, source, key)
            })
            .collect(),
    };
    ReplicationObservation {
        source: source.into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(measurements).unwrap())),
    }
}

fn senders() -> Vec<ReplicationObservation> {
    let mut first = native(
        "pg_stat_replication",
        "42",
        &[
            ("kind", "sender"),
            ("name", "standby_a"),
            ("state", "streaming"),
            ("sync_state", "sync"),
            ("sync_priority", "1"),
            ("current_lsn", "FFFFFFFF/FFFFFFFF"),
            ("sent_lsn", "FFFFFFFF/FFFFFFFE"),
            ("write_lsn", "FFFFFFFF/FFFFFFFD"),
            ("flush_lsn", "FFFFFFFF/FFFFFFFC"),
            ("replay_lsn", "FFFFFFFF/FFFFFFFB"),
            ("write_lag", "00:00:00.123456"),
            ("flush_lag", "00:00:00.234567"),
            ("replay_lag", "00:00:00.345678"),
            ("reply_time", "2026-11-16T11:59:59.123456Z"),
            ("backend_start", "2026-11-15T00:00:00.000001Z"),
            ("active_pid", "42"),
            ("observed_at", "2026-11-16T12:00:00.123456Z"),
            ("server_version_num", "170000"),
            ("full_visibility", "true"),
            ("read_database_oid", "16384"),
            ("read_standby", "false"),
            ("column_availability", r#"{"reply_time":true}"#),
        ],
        &[("behind", 4, CounterUnit::Bytes)],
    );
    let ReplicationFact::Native(sample) = &mut first.fact else { unreachable!() };
    sample.measurements.identity =
        sample.measurements.identity.clone().qualified("backend_start", "2026-11-15T00:00:00.000001Z");
    sample.key = sample.measurements.key().unwrap();
    sample.session = Some(NativeActivityIdentity { id: "42".into(), generation: "2026-11-15T00:00:00.000001Z".into() });
    for (key, value) in [("write_lag_s", 0.123456), ("flush_lag_s", 0.234567), ("lag_s", 0.345678)] {
        let mut counter = NativeCounterSample::gauge(
            key,
            0,
            CounterUnit::Seconds,
            MeasurementScope::Server,
            "pg_stat_replication",
            key,
        );
        counter.value = MeasurementValue::Real(value);
        sample.measurements.counters.push(counter);
    }
    let mut second = native(
        "pg_stat_replication",
        "43",
        &[
            ("kind", "sender"),
            ("name", "standby_b"),
            ("state", "streaming"),
            ("sync_state", "async"),
            ("current_lsn", "FFFFFFFF/FFFFFFFF"),
            ("sent_lsn", "FFFFFFFF/FFFFFFFE"),
            ("column_availability", r#"{"reply_time":true}"#),
            ("server_version_num", "170000"),
            ("full_visibility", "true"),
            ("active_pid", "43"),
            ("backend_start", "2026-11-15T00:00:00.000002Z"),
        ],
        &[],
    );
    let ReplicationFact::Native(sample) = &mut second.fact else { unreachable!() };
    sample.measurements.identity =
        sample.measurements.identity.clone().qualified("backend_start", "2026-11-15T00:00:00.000002Z");
    sample.key = sample.measurements.key().unwrap();
    sample.session = Some(NativeActivityIdentity { id: "43".into(), generation: "2026-11-15T00:00:00.000002Z".into() });
    vec![first, second]
}

fn standby(paused: bool) -> ReplicationObservation {
    let mut row = native(
        "pg_recovery",
        "local",
        &[
            ("kind", "standby"),
            ("name", "this server"),
            ("receiver", "streaming"),
            ("active_pid", "73"),
            ("receive_start_lsn", "2/0"),
            ("receive_start_tli", "5"),
            ("received_tli", "5"),
            ("receive_lsn", "2/FFFFFFF0"),
            ("written_lsn", "2/FFFFFFF0"),
            ("flushed_lsn", "2/FFFFFF00"),
            ("replay_lsn", "2/FFFF0000"),
            ("latest_end_lsn", "3/10"),
            ("last_msg_send_time", "2026-11-16T11:59:59.111111Z"),
            ("last_msg_receipt_time", "2026-11-16T12:00:00.222222Z"),
            ("latest_end_time", "2026-11-16T11:59:59.333333Z"),
            ("last_replay_commit_at", "2026-11-15T12:00:00.444444Z"),
            ("replay_pause_requested", "true"),
            ("replay_pause_state", if paused { "paused" } else { "pause requested" }),
            ("replay_pause_access", r#"{"requested":true,"state":true}"#),
            (
                "column_availability",
                r#"{"receiver_received_lsn":false,"written_lsn":true,"flushed_lsn":true,"replay_pause_state":true}"#,
            ),
            ("streaming_configured", "true"),
            ("server_version_num", "170000"),
            ("full_visibility", "true"),
        ],
        &[
            ("behind", 65_520, CounterUnit::Bytes),
            ("since_replay", 86_400, CounterUnit::Seconds),
            ("delay_s", 0, CounterUnit::Seconds),
        ],
    );
    let ReplicationFact::Native(sample) = &mut row.fact else { unreachable!() };
    sample.unresolved_session_id = Some("73".into());
    row
}

fn slots() -> Vec<ReplicationObservation> {
    let mut invalid = native(
        "pg_replication_slots",
        "consumer_lost",
        &[
            ("kind", "slot"),
            ("name", "consumer_lost"),
            ("slot_name", "consumer_lost"),
            ("slot_type", "logical"),
            ("database", "app"),
            ("database_oid", "16384"),
            ("plugin", "pgoutput"),
            ("active", "false"),
            ("temporary", "false"),
            ("restart_lsn", "0/1234"),
            ("confirmed_flush_lsn", "0/FFFF"),
            ("wal_status", "lost"),
            ("invalidation_reason", "wal_removed"),
            ("synced", "false"),
            ("failover", "false"),
            ("inactive_since", "2026-11-15T12:00:00.123456Z"),
            ("server_version_num", "170000"),
            (
                "column_availability",
                r#"{"wal_status":true,"safe_wal_size":true,"failover":true,"synced":true,"inactive_since":true,"invalidation_reason":true}"#,
            ),
        ],
        &[("kept", 9_007_199_254_740_993, CounterUnit::Bytes), ("inactive_s", 86_400, CounterUnit::Seconds)],
    );
    let ReplicationFact::Native(sample) = &mut invalid.fact else { unreachable!() };
    sample.slot_name = Some("consumer_lost".into());
    let mut synced = native(
        "pg_replication_slots",
        "synced_consumer",
        &[
            ("kind", "slot"),
            ("name", "synced_consumer"),
            ("slot_name", "synced_consumer"),
            ("slot_type", "logical"),
            ("database", "app"),
            ("database_oid", "16384"),
            ("active", "false"),
            ("synced", "true"),
            ("failover", "true"),
            ("wal_status", "reserved"),
            ("restart_lsn", "1/1234"),
            ("confirmed_flush_lsn", "1/FFFF"),
            ("inactive_since", "2026-11-16T11:00:00.123456Z"),
            ("server_version_num", "170000"),
            (
                "column_availability",
                r#"{"wal_status":true,"safe_wal_size":true,"failover":true,"synced":true,"inactive_since":true}"#,
            ),
        ],
        &[
            ("kept", 8_192, CounterUnit::Bytes),
            ("safe_wal_size", 0, CounterUnit::Bytes),
            ("inactive_s", 3_600, CounterUnit::Seconds),
        ],
    );
    let ReplicationFact::Native(sample) = &mut synced.fact else { unreachable!() };
    sample.slot_name = Some("synced_consumer".into());
    vec![invalid, synced]
}

fn legacy_slot() -> ReplicationObservation {
    native(
        "pg_replication_slots",
        "legacy_consumer",
        &[
            ("kind", "slot"),
            ("name", "legacy_consumer"),
            ("slot_name", "legacy_consumer"),
            ("slot_type", "logical"),
            ("database", "app"),
            ("database_oid", "16384"),
            ("active", "false"),
            ("server_version_num", "120000"),
            (
                "column_availability",
                r#"{"wal_status":false,"safe_wal_size":false,"failover":false,"synced":false,"inactive_since":false}"#,
            ),
        ],
        &[("kept", 0, CounterUnit::Bytes)],
    )
}

fn channels(mariadb: bool) -> Vec<ReplicationObservation> {
    let mut first = native(
        "replication-channels",
        if mariadb { "orders_maria" } else { "orders_mysql" },
        &[
            ("channel", "orders"),
            ("source_host", "source.fixture.invalid"),
            ("io_running", "No"),
            ("sql_running", if mariadb { "No" } else { "Yes" }),
            ("source_log_file", "bin.000002"),
            ("relay_source_log_file", "bin.000001"),
            ("relay_log_file", "relay.000042"),
            ("io_error_at_without_offset", "261116 01:02:03"),
            ("sql_error_at_without_offset", "261115 00:59:58"),
            ("source_last_event_at_without_offset", "2026-11-16 01:02:02"),
            ("applied_last_event_at_without_offset", "2026-11-16 00:59:57"),
            ("replicate_do_db", "app"),
            ("replicate_ignore_db", ""),
            ("parallel_mode", if mariadb { "optimistic" } else { "unknown" }),
        ],
        &[
            ("received-source-log-position", 9_007_199_254_740_993, CounterUnit::Bytes),
            ("executed-source-log-position", 9_007_199_254_740_000, CounterUnit::Bytes),
            ("relay-log-position", 4, CounterUnit::Bytes),
            ("configured-delay", 0, CounterUnit::Seconds),
            ("io-error-code", 2003, CounterUnit::Count),
            ("sql-error-code", 0, CounterUnit::Count),
        ],
    );
    if mariadb {
        let ReplicationFact::Native(sample) = &mut first.fact else { unreachable!() };
        for (key, value) in [("ddl-groups", 9_007_199_254_740_993), ("retried-transactions", 0)] {
            let mut counter = NativeCounterSample::gauge(
                key,
                value,
                CounterUnit::Count,
                MeasurementScope::Server,
                "replication-channels",
                key,
            );
            counter.nature = CounterNature::Cumulative;
            sample.measurements.counters.push(counter);
        }
    }
    let second = native(
        "replication-channels",
        "other_channel",
        &[
            ("channel", "other"),
            ("io_running", "Yes"),
            ("sql_running", "No"),
            ("source_host", "different.fixture.invalid"),
        ],
        &[("sql-error-code", 1062, CounterUnit::Count)],
    );
    let mut rows = vec![first, second];
    for row in &mut rows {
        let ReplicationFact::Native(sample) = &mut row.fact else { unreachable!() };
        sample.measurements.identity =
            NativeIdentity::new(serde_json::to_string(&Some(&sample.measurements.properties["channel"])).unwrap());
        sample.key = sample.measurements.key().unwrap();
    }
    rows
}

fn availability_group() -> Vec<ReplicationObservation> {
    let group = AvailabilityGroupContext {
        catalog_observed: true,
        state_observed: true,
        name: Some("orders_ag".into()),
        primary_replica: Some("primary.fixture.invalid".into()),
        primary_recovery_health: Some(1),
        native_primary_recovery_health: Some("ONLINE".into()),
        secondary_recovery_health: Some(1),
        native_secondary_recovery_health: Some("ONLINE".into()),
        synchronization_health: Some(1),
        native_synchronization_health: Some("PARTIALLY_HEALTHY".into()),
        failure_condition_level: Some(3),
        health_check_timeout_ms: Some("30000".into()),
        automated_backup_preference: Some(2),
        native_automated_backup_preference: Some("SECONDARY".into()),
        is_distributed: Some(false),
        is_basic: Some(false),
        dtc_support: Some(false),
        database_failover: Some(true),
        cluster_type: Some(0),
        native_cluster_type: Some("WSFC".into()),
        required_synchronized_secondaries_to_commit: Some("1".into()),
        configuration_sequence: Some("9007199254740993".into()),
    };
    let replica = AvailabilityReplicaContext {
        catalog_observed: true,
        state_observed: true,
        role: Some(2),
        native_role: Some("SECONDARY".into()),
        operational_state: Some(2),
        native_operational_state: Some("ONLINE".into()),
        connected_state: Some(0),
        native_connected_state: Some("DISCONNECTED".into()),
        recovery_health: Some(1),
        native_recovery_health: Some("ONLINE".into()),
        synchronization_health: Some(0),
        native_synchronization_health: Some("NOT_HEALTHY".into()),
        last_connect_error_number: Some(35201),
        last_connect_error_without_offset: Some("2026-11-16T01:02:03.123".into()),
        failover_mode: Some(1),
        native_failover_mode: Some("MANUAL".into()),
        session_timeout_seconds: Some("10".into()),
        primary_role_allow_connections: Some(3),
        native_primary_role_allow_connections: Some("ALL".into()),
        secondary_role_allow_connections: Some(2),
        native_secondary_role_allow_connections: Some("ALL".into()),
        seeding_mode: Some(0),
        native_seeding_mode: Some("AUTOMATIC".into()),
        endpoint_url: Some("TCP://secondary.fixture.invalid:5022".into()),
        read_only_routing_url: Some("TCP://secondary.fixture.invalid:1433".into()),
        read_write_routing_url: None,
        backup_priority: Some(50),
        created_without_offset: Some("2026-11-10T00:00:00.000".into()),
        modified_without_offset: Some("2026-11-15T00:00:00.003".into()),
    };
    let point = AvailabilityReplicaSample {
        group_id: "11111111-1111-1111-1111-111111111111".into(),
        group_database_id: "22222222-2222-2222-2222-222222222222".into(),
        replica_id: "33333333-3333-3333-3333-333333333333".into(),
        database_id: Some(7),
        database_name: Some("app".into()),
        group: Some(Box::new(group)),
        replica: Some(Box::new(replica)),
        native_name: Some("secondary.fixture.invalid".into()),
        is_local: Some(false),
        is_primary: Some(false),
        state: Some(0),
        native_state: Some("NOT SYNCHRONIZING".into()),
        mode: Some(0),
        native_mode: Some("ASYNCHRONOUS_COMMIT".into()),
        is_suspended: Some(true),
        suspend_reason: Some(0),
        native_suspend_reason: Some("SUSPEND_FROM_USER".into()),
        synchronization_health: Some(0),
        native_synchronization_health: Some("NOT_HEALTHY".into()),
        log_send_queue_kb: Some("9007199254740993".into()),
        redo_queue_kb: Some("0".into()),
        log_send_rate_kb_per_second: Some("0".into()),
        redo_rate_kb_per_second: None,
        recovery_lsn: Some("1234567890123456789012345".into()),
        truncation_lsn: Some("1234567890123456789012300".into()),
        last_sent_lsn: Some("1234567890123456789012340".into()),
        last_received_lsn: Some("1234567890123456789012330".into()),
        last_hardened_lsn: Some("1234567890123456789012320".into()),
        last_redone_lsn: Some("1234567890123456789012310".into()),
        end_of_log_lsn: Some("1234567890123456789012345".into()),
        last_commit_lsn: Some("1234567890123456789012300".into()),
        last_sent_without_offset: Some("2026-11-16T01:02:03.123".into()),
        last_received_without_offset: Some("2026-11-16T01:02:03.127".into()),
        last_hardened_without_offset: Some("2026-11-16T01:02:03.130".into()),
        last_redone_without_offset: Some("2026-11-16T01:02:03.133".into()),
        last_commit_without_offset: Some("2026-11-16T01:02:03.137".into()),
        ..Default::default()
    };
    vec![ReplicationObservation {
        source: "availability-group-state".into(),
        fact: ReplicationFact::AvailabilityGroup(Box::new(point)),
    }]
}

fn geo_links() -> Vec<ReplicationObservation> {
    vec![ReplicationObservation {
        source: "geo-replication-links".into(),
        fact: ReplicationFact::GeoLink(Box::new(GeoReplicationSample {
            link_id: "44444444-4444-4444-4444-444444444444".into(),
            partner_server: Some("geo.fixture.invalid".into()),
            partner_database: Some("app".into()),
            role: Some(0),
            native_role: Some("PRIMARY".into()),
            state: Some(2),
            native_state: Some("CATCH_UP".into()),
            lag_seconds: Some("0".into()),
            last_replication_at: Some("2026-11-16T01:02:03.123+02:00".into()),
            secondary_allow_connections: Some(2),
            native_secondary_allow_connections: Some("ALL".into()),
            last_commit_at: Some("2026-11-16T01:02:04.127+02:00".into()),
        })),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::SourceState;

    #[test]
    fn every_case_seals_through_original_capture_and_native_receipts_without_changing_facts() {
        let catalog = crate::analytical::catalog::LogicalCatalog::new().unwrap();
        for (sequence, case) in ReplicationCase::ALL.iter().copied().enumerate() {
            let (source, records) = replication_evidence(case);
            let recovery = Recovery::capture(
                "replication:fixture",
                catalog.digest(),
                source.clone(),
                records.clone(),
                sequence as u64 + 200_000,
            );
            recovery.manifest.validate().unwrap();
            let batch = &recovery.manifest.batches[0];
            assert_eq!(batch.stream, TelemetryStream::ReplicationSamples);
            assert_eq!(batch.rows.len(), records.len());
            for (record, receipt) in records.into_iter().zip(&batch.rows) {
                let original = ExportedRecord::new(&source, &recovery.manifest.capture, record).unwrap();
                assert_eq!(RowReceipt::from_export(&original, receipt.key.row_ordinal).unwrap(), *receipt);
            }
        }
    }

    #[test]
    fn changed_source_counts_bounds_and_native_key_bindings_are_not_repaired() {
        let (source, records) = replication_evidence(ReplicationCase::PostgresSender);
        let points: Vec<_> = records
            .into_iter()
            .map(|record| {
                let TelemetryRecord::Replication(row) = record else { unreachable!() };
                row
            })
            .collect();
        for change in [0, 1, 2] {
            let mut source = source.clone();
            let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
            match change {
                0 => header.observed_until = "2019-01-01T00:00:00Z".into(),
                1 => header.sources[0].records += 1,
                _ => header.sources[0].state = SourceState::Empty,
            }
            assert!(ReplicationSnapshot::from_source(&source, points.clone()).is_err());
        }
        let mut wrong = points.clone();
        wrong[0].source = "other-source".into();
        assert!(ReplicationSnapshot::from_source(&source, wrong).is_err());
        let mut wrong = points;
        let ReplicationFact::Native(sample) = &mut wrong[0].fact else { unreachable!() };
        sample.measurements.identity = NativeIdentity::new("replacement");
        assert!(ReplicationSnapshot::from_source(&source, wrong).is_err());
    }
}
