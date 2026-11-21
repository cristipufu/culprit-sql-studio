use super::*;
use crate::{DatabaseIdentity, ErrorCode, MeasurementScope, ResetMarker, telemetry::*};
use std::collections::BTreeMap;

const PRIVATE: &str = "REPLICATION-PRIVATE";
const FROM: &str = "2026-11-16T10:00:00.123456Z";
const UNTIL: &str = "2026-11-16T10:00:01.123456Z";
const STARTED: &str = "2026-11-01T08:00:00.123456Z";

fn snapshot(engine: Engine, read: ReplicationSnapshot) -> Snapshot {
    let text = match engine {
        Engine::Mssql => include_str!("../../../../../fixtures/mssql-demo.json"),
        Engine::Postgres => include_str!("../../../../../fixtures/postgres-demo.json"),
        Engine::Mysql => include_str!("../../../../../fixtures/mysql-demo.json"),
        _ => panic!("the test has no fixture for this engine"),
    };
    let mut snapshot: Snapshot = serde_json::from_str(text).unwrap();
    snapshot.pressure.get_or_insert_default().replication = Some(Box::new(read));
    snapshot
}

fn native_context(engine: Engine) -> ObservationReadContext {
    let mut target = DatabaseIdentity::server(engine, "shop", "REPLICATION-PRIVATE-read.example", 15433, None);
    target.incarnation = Some("REPLICATION-PRIVATE-database-generation".into());
    ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: target.into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("REPLICATION-PRIVATE-namespace"),
                    native: NativeIdentity::new("REPLICATION-PRIVATE-member")
                        .qualified("endpoint", "REPLICATION-PRIVATE-member.example"),
                }),
                epoch: Some("REPLICATION-PRIVATE-native-epoch".into()),
            },
            observed_from: FROM.into(),
            observed_until: UNTIL.into(),
            database_writable: Some(false),
        }),
    }
}

fn carrier(context: ObservationReadContext, observations: Vec<ReplicationObservation>) -> ReplicationSnapshot {
    let mut counts = BTreeMap::<String, u32>::new();
    for row in &observations {
        *counts.entry(row.source.clone()).or_default() += 1;
    }
    ReplicationSnapshot {
        mssql_scope: None,
        context,
        sources: counts
            .into_iter()
            .map(|(source, count)| StateSourceCoverage::observed(source, count, false))
            .collect(),
        observations,
    }
}

fn availability() -> AvailabilityReplicaSample {
    AvailabilityReplicaSample {
        group_id: "11111111-1111-1111-1111-111111111111".into(),
        group_database_id: "22222222-2222-2222-2222-222222222222".into(),
        replica_id: "33333333-3333-3333-3333-333333333333".into(),
        database_id: Some(5),
        database_name: Some("REPLICATION-PRIVATE-database".into()),
        native_name: Some("REPLICATION-PRIVATE-replica.example".into()),
        is_local: Some(false),
        group: Some(Box::new(AvailabilityGroupContext {
            catalog_observed: true,
            state_observed: true,
            name: Some("REPLICATION-PRIVATE-group".into()),
            primary_replica: Some("REPLICATION-PRIVATE-primary.example".into()),
            primary_recovery_health: Some(1),
            native_primary_recovery_health: Some("ONLINE".into()),
            secondary_recovery_health: None,
            native_secondary_recovery_health: None,
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
            cluster_type: Some(2),
            native_cluster_type: Some("EXTERNAL".into()),
            required_synchronized_secondaries_to_commit: Some("0".into()),
            configuration_sequence: Some("9007199254740993".into()),
        })),
        replica: Some(Box::new(AvailabilityReplicaContext {
            catalog_observed: true,
            state_observed: true,
            role: Some(2),
            native_role: Some("SECONDARY".into()),
            operational_state: None,
            native_operational_state: None,
            connected_state: Some(0),
            native_connected_state: Some("DISCONNECTED".into()),
            recovery_health: Some(0),
            native_recovery_health: Some("ONLINE_IN_PROGRESS".into()),
            synchronization_health: Some(0),
            native_synchronization_health: Some("NOT_HEALTHY".into()),
            last_connect_error_number: Some(35206),
            last_connect_error_without_offset: Some("2026-11-16T08:59:00.0030000".into()),
            failover_mode: Some(1),
            native_failover_mode: Some("MANUAL".into()),
            session_timeout_seconds: Some("10".into()),
            primary_role_allow_connections: Some(3),
            native_primary_role_allow_connections: Some("ALL".into()),
            secondary_role_allow_connections: Some(1),
            native_secondary_role_allow_connections: Some("READ_ONLY".into()),
            seeding_mode: Some(0),
            native_seeding_mode: Some("AUTOMATIC".into()),
            endpoint_url: Some(
                "TCP://REPLICATION-PRIVATE-user:REPLICATION-PRIVATE-password@replica.example:5022".into(),
            ),
            read_only_routing_url: Some("TCP://REPLICATION-PRIVATE-read.example:1433".into()),
            read_write_routing_url: Some("TCP://REPLICATION-PRIVATE-write.example:1433".into()),
            backup_priority: Some(50),
            created_without_offset: Some("2026-11-01T09:00:00.0030000".into()),
            modified_without_offset: None,
        })),
        progress: MssqlReplicaDatabaseProgress {
            is_primary: Some(false),
            state: Some(0),
            native_state: Some("NOT SYNCHRONIZING".into()),
            is_suspended: Some(true),
            suspend_reason: Some(2),
            native_suspend_reason: Some("SUSPEND_FROM_REDO".into()),
            synchronization_health: Some(0),
            native_synchronization_health: Some("NOT_HEALTHY".into()),
            log_send_queue_kb: Some("9007199254740993".into()),
            redo_queue_kb: Some("0".into()),
            recovery_lsn: Some("9999999999999999999999999".into()),
            last_sent_lsn: Some("9007199254740993".into()),
            last_redone_lsn: Some("0".into()),
            last_sent_without_offset: Some("2026-11-16T09:00:00.0030000".into()),
            last_commit_without_offset: Some("2026-11-16T09:00:00.1234567".into()),
            ..Default::default()
        },

        ..Default::default()
    }
}

fn geo() -> GeoReplicationSample {
    GeoReplicationSample {
        link_id: "44444444-4444-4444-4444-444444444444".into(),
        partner_server: Some("REPLICATION-PRIVATE-geo.example".into()),
        partner_database: Some("REPLICATION-PRIVATE-geo-database".into()),
        role: Some(1),
        native_role: Some("SECONDARY".into()),
        state: Some(4),
        native_state: Some("SUSPENDED".into()),
        secondary_allow_connections: Some(0),
        native_secondary_allow_connections: Some("No".into()),
        last_commit_at: Some("2026-11-16T09:00:00.123456700Z".into()),
        ..Default::default()
    }
}

fn shared_read(snapshot: &Snapshot) -> &ReplicationSnapshot {
    snapshot.pressure.as_ref().unwrap().replication.as_ref().unwrap()
}

fn assert_private_and_idempotent(original: &Snapshot, shared: &Snapshot) {
    assert!(serde_json::to_string(original).unwrap().contains(PRIVATE), "the original must exercise private fields");
    assert!(!serde_json::to_string(shared).unwrap().contains(PRIVATE));
    assert_eq!(serde_json::to_value(shared).unwrap(), serde_json::to_value(redact_snapshot(shared)).unwrap());
}

#[test]
fn availability_and_geo_names_and_complete_urls_are_private_without_changing_native_facts() {
    let rows = vec![
        ReplicationObservation {
            source: "availability-replicas".into(),
            fact: ReplicationFact::AvailabilityGroup(Box::new(availability())),
        },
        ReplicationObservation { source: "geo-links".into(), fact: ReplicationFact::GeoLink(Box::new(geo())) },
    ];
    let read = carrier(native_context(Engine::Mssql), rows);
    assert!(read.is_valid());
    let original = snapshot(Engine::Mssql, read);
    let before = serde_json::to_value(&original).unwrap();
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    assert_eq!(before, serde_json::to_value(&original).unwrap(), "sharing cannot modify original captured facts");
    let original_read = shared_read(&original);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    assert_eq!(read.sources, original_read.sources);
    assert_eq!(read.context.bounds(), original_read.context.bounds());
    let context = read.context.native().unwrap();
    assert!(context.has_qualified_local_owner());
    assert_eq!(context.database_writable, Some(false));
    let ReplicationFact::AvailabilityGroup(masked) = &read.observations[0].fact else { panic!() };
    let mut expected = availability();
    expected.database_name = masked.database_name.clone();
    expected.native_name = masked.native_name.clone();
    for name in [&expected.database_name, &expected.native_name] {
        assert!(name.as_ref().unwrap().starts_with("redacted-"));
    }
    let group = expected.group.as_mut().unwrap();
    group.name = masked.group.as_ref().unwrap().name.clone();
    group.primary_replica = masked.group.as_ref().unwrap().primary_replica.clone();
    let replica = expected.replica.as_mut().unwrap();
    replica.endpoint_url = masked.replica.as_ref().unwrap().endpoint_url.clone();
    replica.read_only_routing_url = masked.replica.as_ref().unwrap().read_only_routing_url.clone();
    replica.read_write_routing_url = masked.replica.as_ref().unwrap().read_write_routing_url.clone();
    assert_eq!(masked.as_ref(), &expected, "all counters, codes, clocks, visibility and GUID keys stay exact");
    let ReplicationFact::GeoLink(masked) = &read.observations[1].fact else { panic!() };
    let mut expected = geo();
    expected.partner_server = masked.partner_server.clone();
    expected.partner_database = masked.partner_database.clone();
    assert_eq!(masked.as_ref(), &expected);
    assert!(masked.lag_seconds.is_none() && masked.last_replication_at.is_none(), "secondary unknowns remain unknown");
}

fn native_row(source: &str, id: &str, values: &[(&str, &str)]) -> ReplicationObservation {
    let set = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new(source).qualified("native_id", id).qualified("backend_start", STARTED),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "kept",
            9_007_199_254_740_993,
            CounterUnit::Bytes,
            MeasurementScope::Server,
            source,
            "kept",
        )],
        properties: values.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect(),
    };
    ReplicationObservation {
        source: source.into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(set).unwrap())),
    }
}

fn native_sample(row: &ReplicationObservation) -> &NativeReplicationSample {
    let ReplicationFact::Native(sample) = &row.fact else { panic!() };
    sample
}

fn native_sample_mut(row: &mut ReplicationObservation) -> &mut NativeReplicationSample {
    let ReplicationFact::Native(sample) = &mut row.fact else { panic!() };
    sample
}

#[test]
fn native_links_rekey_consistently_without_losing_measurements_source_outcomes_or_visibility() {
    let mut sender = native_row(
        "pg_stat_replication",
        "REPLICATION-PRIVATE-pid",
        &[
            ("kind", "sender"),
            ("name", "REPLICATION-PRIVATE-client.example"),
            ("active_pid", "REPLICATION-PRIVATE-pid"),
            ("backend_start", STARTED),
            ("slot_name", "REPLICATION-PRIVATE-slot"),
            ("database", "REPLICATION-PRIVATE-database"),
            ("state", "streaming"),
            ("sync_state", "async"),
            ("backend_xmin", "9007199254740993"),
            ("current_lsn", "FFFFFFFF/FFFFFFFF"),
            ("replay_lsn", "0/0"),
            ("reply_time", FROM),
            ("write_lag", "00:00:00.123456"),
            ("column_availability", "{\"reply_time\":true}"),
            ("full_visibility", "false"),
        ],
    );
    let mut slot = native_row(
        "pg_replication_slots",
        "REPLICATION-PRIVATE-slot",
        &[
            ("kind", "slot"),
            ("name", "REPLICATION-PRIVATE-slot"),
            ("slot_name", "REPLICATION-PRIVATE-slot"),
            ("active_pid", "REPLICATION-PRIVATE-pid"),
            ("backend_start", STARTED),
            ("database_oid", "REPLICATION-PRIVATE-oid"),
            ("plugin", "/REPLICATION-PRIVATE/private-plugin"),
            ("wal_status", "lost"),
            ("invalidation_reason", "wal_removed"),
            ("conflicting", "false"),
            ("inactive_since", FROM),
            ("restart_lsn", "16/B374D848"),
            ("kept_native", "-9007199254740993"),
            (
                "REPLICATION-PRIVATE-unrecognized-property",
                "tcp://REPLICATION-PRIVATE-user:REPLICATION-PRIVATE-secret@host",
            ),
        ],
    );
    for row in [&mut sender, &mut slot] {
        let sample = native_sample_mut(row);
        sample.session =
            Some(NativeActivityIdentity { id: "REPLICATION-PRIVATE-pid".into(), generation: STARTED.into() });
        sample.slot_name = Some("REPLICATION-PRIVATE-slot".into());
        sample.measurements.counters[0].reset = ResetMarker::At(STARTED.into());
        sample.measurements.counters[0].native_type = Some("272696576".into());
    }
    let mut unresolved = native_row(
        "pg_recovery",
        "local",
        &[
            ("kind", "standby"),
            ("active_pid", "REPLICATION-PRIVATE-unresolved"),
            ("receiver", "stopped"),
            ("replay_pause_state", "paused"),
            ("streaming_configured", "false"),
            ("replay_pause_access", "{\"paused\":true,\"state\":null}"),
        ],
    );
    native_sample_mut(&mut unresolved).unresolved_session_id = Some("REPLICATION-PRIVATE-unresolved".into());
    let mut read = carrier(native_context(Engine::Postgres), vec![sender, slot, unresolved]);
    read.sources.push(StateSourceCoverage::unavailable("denied-family".into(), ErrorCode::PermissionDenied, None));
    assert!(read.is_valid());
    let original = snapshot(Engine::Postgres, read);
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    let input = shared_read(&original);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    assert_eq!(read.sources, input.sources);
    for (raw, masked) in input.observations.iter().zip(&read.observations) {
        assert_eq!(raw.source, masked.source);
        let raw = native_sample(raw);
        let masked = native_sample(masked);
        assert_ne!(masked.key, raw.key);
        assert_eq!(masked.key, masked.measurements.key().unwrap());
        assert_eq!(masked.measurements.counters, raw.measurements.counters);
        assert_eq!(masked.measurements.scope, raw.measurements.scope);
    }
    let sender = native_sample(&read.observations[0]);
    let slot = native_sample(&read.observations[1]);
    assert_eq!(sender.session, slot.session);
    assert_eq!(sender.slot_name, slot.slot_name);
    assert_eq!(sender.slot_name.as_deref(), Some(sender.measurements.properties["slot_name"].as_str()));
    assert_eq!(slot.slot_name.as_deref(), Some(slot.measurements.properties["name"].as_str()));
    let session = sender.session.as_ref().unwrap();
    assert_eq!(session.id, sender.measurements.properties["active_pid"]);
    assert_eq!(session.id, sender.measurements.identity.qualifiers["native_id"]);
    assert_eq!(session.generation, sender.measurements.identity.qualifiers["backend_start"]);
    assert_eq!(sender.measurements.properties["backend_start"], STARTED);
    assert_eq!(sender.measurements.properties["column_availability"], "{\"reply_time\":true}");
    assert_eq!(sender.measurements.properties["full_visibility"], "false");
    assert_eq!(slot.measurements.properties["wal_status"], "lost");
    assert_eq!(slot.measurements.properties["invalidation_reason"], "wal_removed");
    assert_eq!(slot.measurements.properties["kept_native"], "-9007199254740993");
    let unresolved = native_sample(&read.observations[2]);
    assert!(unresolved.session.is_none());
    assert_eq!(
        unresolved.unresolved_session_id.as_deref(),
        Some(unresolved.measurements.properties["active_pid"].as_str())
    );
    assert_eq!(unresolved.measurements.properties["receiver"], "stopped");
    assert_eq!(unresolved.measurements.properties["replay_pause_state"], "paused");
}

#[test]
fn mysql_channel_files_and_filters_are_private_while_native_codes_clocks_and_counter_semantics_remain_exact() {
    let mut row = native_row(
        "replication-channels",
        "\"REPLICATION-PRIVATE-channel\"",
        &[
            ("channel", "REPLICATION-PRIVATE-channel"),
            ("source_host", "REPLICATION-PRIVATE-source.example"),
            ("source_uuid", "REPLICATION-PRIVATE-source-uuid"),
            ("source_log_file", "/REPLICATION-PRIVATE/mysql-bin.000001"),
            ("relay_source_log_file", "/REPLICATION-PRIVATE/mysql-bin.000001"),
            ("relay_log_file", "/REPLICATION-PRIVATE/relay.000002"),
            ("until_log_file", "/REPLICATION-PRIVATE/mysql-bin.000001"),
            ("replicate_do_table", "REPLICATION-PRIVATE-db.REPLICATION-PRIVATE-table"),
            ("replicate_rewrite_db", "((REPLICATION-PRIVATE-old,REPLICATION-PRIVATE-new))"),
            ("io_running", "Connecting"),
            ("sql_running", "No"),
            ("using_gtid", "Slave_Pos"),
            ("io_state", "Connecting to tcp://REPLICATION-PRIVATE-user:REPLICATION-PRIVATE-password@source"),
            ("io_error_at_without_offset", "261116 10:00:00"),
            ("source_last_event_at_without_offset", "2026-11-16 10:00:00"),
            ("withheld_fields", "replicate_wild_do_table,replicate_wild_ignore_table"),
        ],
    );
    let sample = native_sample_mut(&mut row);
    sample.measurements.counters = vec![
        NativeCounterSample::gauge(
            "io-error-code",
            1045,
            CounterUnit::Count,
            MeasurementScope::Server,
            "replication-channels",
            "last_io_errno",
        ),
        NativeCounterSample::gauge(
            "source-port",
            3307,
            CounterUnit::Count,
            MeasurementScope::Server,
            "replication-channels",
            "source_port",
        ),
        NativeCounterSample::gauge(
            "executed-log-entries",
            9_007_199_254_740_993,
            CounterUnit::Count,
            MeasurementScope::Server,
            "replication-channels",
            "executed_log_entries",
        ),
    ];
    sample.measurements.counters[2].nature = CounterNature::Cumulative;
    sample.measurements.counters[2].reset = ResetMarker::Unknown;
    let default =
        native_row("replication-channels", "\"\"", &[("channel", ""), ("io_running", "Yes"), ("sql_running", "Yes")]);
    let context = ObservationReadContext::Collector { observed_from: FROM.into(), observed_until: UNTIL.into() };
    let read = carrier(context, vec![row, default]);
    assert!(read.is_valid());
    let original = snapshot(Engine::Mysql, read);
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    let input = shared_read(&original);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    assert_eq!(read.context, input.context);
    assert!(read.context.native().is_none(), "collector clocks cannot become native continuity proof");
    assert_eq!(read.sources, input.sources);
    let raw = native_sample(&input.observations[0]);
    let masked = native_sample(&read.observations[0]);
    assert_eq!(masked.measurements.counters, raw.measurements.counters);
    for name in [
        "io_running",
        "sql_running",
        "using_gtid",
        "io_error_at_without_offset",
        "source_last_event_at_without_offset",
        "withheld_fields",
    ] {
        assert_eq!(masked.measurements.properties[name], raw.measurements.properties[name], "{name}");
    }
    assert_eq!(
        masked.measurements.properties["source_log_file"],
        masked.measurements.properties["relay_source_log_file"]
    );
    assert_eq!(masked.measurements.properties["source_log_file"], masked.measurements.properties["until_log_file"]);
    assert_eq!(
        native_sample(&read.observations[1]).measurements.properties["channel"],
        "",
        "the default channel stays distinct from unavailable"
    );
}

#[test]
fn unknown_properties_and_counter_owner_labels_cannot_smuggle_endpoints_through_known_measurement_fields() {
    let mut row = native_row(
        "pg_replication_slots",
        "REPLICATION-PRIVATE-slot",
        &[
            ("kind", "slot"),
            ("invalidation_reason", "REPLICATION-PRIVATE-future-reason"),
            ("replay_pause_state", "tcp://REPLICATION-PRIVATE-user:REPLICATION-PRIVATE-password@host"),
            ("column_availability", "{\"REPLICATION-PRIVATE-column\":true}"),
            ("observed_at", "tcp://REPLICATION-PRIVATE-user@host"),
            ("REPLICATION-PRIVATE-property", "REPLICATION-PRIVATE-value"),
        ],
    );
    let counter = &mut native_sample_mut(&mut row).measurements.counters[0];
    counter.reset = ResetMarker::Generation("REPLICATION-PRIVATE-reset".into());
    counter.native_instance = Some("REPLICATION-PRIVATE-instance".into());
    counter.native_type = Some("REPLICATION-PRIVATE-type".into());
    counter.native_name = "tcp://REPLICATION-PRIVATE-user@host".into();
    let read = carrier(native_context(Engine::Postgres), vec![row]);
    assert!(read.is_valid());
    let original = snapshot(Engine::Postgres, read);
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    let sample = native_sample(&read.observations[0]);
    assert!(
        !sample.measurements.properties["invalidation_reason"].is_empty(),
        "a recorded invalidation cannot become absent"
    );
    let counter = &sample.measurements.counters[0];
    assert_eq!(counter.value, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(counter.unit, CounterUnit::Bytes);
    assert_eq!(counter.nature, CounterNature::Gauge);
    assert!(matches!(&counter.reset, ResetMarker::Generation(value) if value.starts_with("redacted-")));
    assert!(counter.native_instance.as_ref().unwrap().starts_with("redacted-"));
}

#[test]
fn unavailable_sources_and_missing_archive_rows_keep_their_original_counts_and_unknown_scope() {
    let mut read = carrier(
        ObservationReadContext::Collector { observed_from: FROM.into(), observed_until: UNTIL.into() },
        vec![native_row(
            "pg_replication_slots",
            "REPLICATION-PRIVATE-slot",
            &[("kind", "slot"), ("wal_status", "unreserved")],
        )],
    );
    read.sources[0].records = 2;
    read.sources.push(StateSourceCoverage::unavailable("denied-family".into(), ErrorCode::PermissionDenied, None));
    assert!(read.is_valid_read(false) && !read.is_valid());
    let original = snapshot(Engine::Postgres, read);
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    let read = shared_read(&shared);
    assert!(read.is_valid_read(false) && !read.is_valid());
    assert_eq!(read.sources, shared_read(&original).sources);
    assert!(read.context.native().is_none());
    assert_eq!(read.observations.len(), 1);
    assert_eq!(native_sample(&read.observations[0]).measurements.properties["wal_status"], "unreserved");
}

#[test]
fn an_unavailable_native_source_cannot_gain_a_member_generation_or_positive_points_during_sharing() {
    let mut context = native_context(Engine::Mssql);
    let ObservationReadContext::Native { context: native } = &mut context else { panic!() };
    native.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    native.database_writable = None;
    let read = ReplicationSnapshot {
        mssql_scope: None,
        context,
        sources: vec![StateSourceCoverage::unavailable(
            "availability-replicas".into(),
            ErrorCode::PermissionDenied,
            None,
        )],
        observations: vec![],
    };
    assert!(read.is_valid());
    let original = snapshot(Engine::Mssql, read);
    let shared = redact_snapshot(&original);
    assert_private_and_idempotent(&original, &shared);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    assert!(read.observations.is_empty());
    assert_eq!(read.sources, shared_read(&original).sources);
    let context = read.context.native().unwrap();
    assert_eq!(context.partition, SourcePartition::InstanceLocal { member: None, epoch: None });
    assert!(context.database_writable.is_none() && !context.has_qualified_local_owner());
}

#[test]
fn an_invalid_original_native_key_is_withheld_instead_of_becoming_valid_through_rekeying() {
    let mut row = native_row("pg_replication_slots", "REPLICATION-PRIVATE-slot", &[("kind", "slot")]);
    native_sample_mut(&mut row).key = "0".repeat(64);
    let read = carrier(native_context(Engine::Postgres), vec![row]);
    assert!(!read.is_valid_read(false));
    let original = snapshot(Engine::Postgres, read);
    let shared = redact_snapshot(&original);
    assert!(shared.pressure.as_ref().unwrap().replication.is_none());
    assert_private_and_idempotent(&original, &shared);
    assert!(!shared_read(&original).is_valid_read(false), "sharing does not repair the original receipt/key mismatch");
}

#[test]
fn sharing_withdraws_indivisible_hyperscale_native_scope_and_points_while_preserving_other_replication_history() {
    let mut context = native_context(Engine::Mssql);
    let ObservationReadContext::Native { context: native } = &mut context else { panic!() };
    let local = "91487244-730d-4b83-8e49-d482241c7288";
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native.partition else { panic!() };
    member.kind = SharedResourceKind::Node;
    member.native = NativeIdentity::new(local).qualified("kind", "azure-sql-hyperscale-replica");
    member.namespace = NativeIdentity::new(native.target.database().unwrap().incarnation.as_ref().unwrap())
        .qualified("kind", "azure-sql-logical-database");
    let point = MssqlHyperscaleReplica {
        selected_database_id: 7,
        observer_replica_id: local.into(),
        database_writable: false,
        observed_at: FROM.into(),
        replica_id: "11111111-1111-1111-1111-111111111111".into(),
        replica_server_name: Some(format!("{PRIVATE}-logical-server")),
        replica_database_name: Some(format!("{PRIVATE}-database")),
        is_local: Some(false),
        replica_role: Some(1),
        native_role: Some("HA SECONDARY".into()),
        local_database_id: Some(70),
        parent_replica_id: Some(local.into()),
    };
    let mut read = carrier(
        context,
        vec![
            ReplicationObservation {
                source: MSSQL_HYPERSCALE_REPLICAS_SOURCE.into(),
                fact: ReplicationFact::HyperscaleReplica(Box::new(point)),
            },
            ReplicationObservation { source: "geo-links".into(), fact: ReplicationFact::GeoLink(Box::new(geo())) },
        ],
    );
    read.mssql_scope = Some(Box::new(MssqlReplicationScope { database_id: 7, replica_id: local.into() }));
    assert!(read.is_valid());
    let original = snapshot(Engine::Mssql, read);
    let shared = redact_snapshot(&original);
    let redacted = shared_read(&shared);
    assert!(redacted.is_valid());
    assert!(redacted.mssql_scope.is_none());
    assert_eq!(redacted.observations.len(), 1);
    assert!(matches!(redacted.observations[0].fact, ReplicationFact::GeoLink(_)));
    let source = redacted.sources.iter().find(|source| source.source == MSSQL_HYPERSCALE_REPLICAS_SOURCE).unwrap();
    assert_eq!(source.state, crate::SourceState::Unavailable);
    assert_eq!(source.records, 0);
    assert!(!source.complete_scope);
    assert_private_and_idempotent(&original, &shared);
}

#[test]
fn sharing_withdraws_azure_local_progress_and_indivisible_native_scope_without_turning_it_into_healthy_zero() {
    let mut context = native_context(Engine::Mssql);
    let ObservationReadContext::Native { context: native } = &mut context else { panic!() };
    let replica_id = "91487244-730d-4b83-8e49-d482241c7288";
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native.partition else { panic!() };
    member.kind = SharedResourceKind::Node;
    member.native = NativeIdentity::new(replica_id).qualified("kind", "azure-sql-hyperscale-replica");
    member.namespace = NativeIdentity::new(native.target.database().unwrap().incarnation.as_ref().unwrap())
        .qualified("kind", "azure-sql-logical-database");
    let point = MssqlAzureReplicaState {
        database_id: 7,
        replica_id: replica_id.into(),
        is_local: true,
        database_writable: false,
        observed_at: FROM.into(),
        group_id: None,
        group_database_id: None,
        progress: MssqlReplicaDatabaseProgress { redo_queue_kb: Some("9007199254740993".into()), ..Default::default() },
    };
    let mut read = carrier(
        context,
        vec![ReplicationObservation {
            source: MSSQL_AZURE_REPLICA_STATE_SOURCE.into(),
            fact: ReplicationFact::AzureReplicaState(Box::new(point)),
        }],
    );
    read.mssql_scope = Some(Box::new(MssqlReplicationScope { database_id: 7, replica_id: replica_id.into() }));
    assert!(read.is_valid());
    let original = snapshot(Engine::Mssql, read);
    let shared = redact_snapshot(&original);
    let read = shared_read(&shared);
    assert!(read.is_valid());
    assert!(read.mssql_scope.is_none());
    assert!(read.observations.is_empty());
    assert_eq!(read.sources[0].state, crate::SourceState::Unavailable);
    assert_eq!(read.sources[0].records, 0);
    assert!(!read.sources[0].complete_scope);
    assert_private_and_idempotent(&original, &shared);
}
