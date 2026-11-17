//! One provider-free native-counter corpus serves warehouse and SDK conformance without another reader/parser.
use super::*;
use culprit_core::{ConnectionOutcomeSnapshot, Engine, MeasurementScope, ResetMarker, SourceState};
use std::collections::BTreeMap;

pub fn connection_outcome_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let postgres = postgres_row(true, 180000, ResetMarker::At("2020-01-01T00:00:00Z".into()));
    let unknown = postgres_row(true, 180000, ResetMarker::Unknown);
    let unrelated = point(
        "pg_stat_database_conflicts",
        NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new("pg_stat_database_conflicts").qualified("dbid", "16384"),
            scope: MeasurementScope::Database,
            counters: vec![counter(
                "pg_stat_database_conflicts",
                "confl_lock",
                3,
                CounterNature::Cumulative,
                MeasurementScope::Database,
                ResetMarker::Unknown,
            )],
            properties: BTreeMap::new(),
        },
    );
    let mysql = mysql_rows();
    vec![
        capture(Engine::Postgres, vec![postgres.clone(), unrelated], SourceState::Collected),
        capture(Engine::Postgres, vec![unknown], SourceState::Collected),
        capture(Engine::Postgres, vec![postgres_row(false, 130000, ResetMarker::Unknown)], SourceState::Collected),
        // Actual field presence wins over a supporting version label, including compatible/backported layouts.
        capture(Engine::Postgres, vec![postgres_row(true, 130000, ResetMarker::Unknown)], SourceState::Collected),
        capture(Engine::Postgres, vec![postgres], SourceState::Partial),
        capture(Engine::Postgres, vec![], SourceState::Unavailable),
        capture(Engine::Mysql, mysql.clone(), SourceState::Collected),
        capture(Engine::Mysql, mysql, SourceState::Partial),
        capture(Engine::Mysql, vec![], SourceState::Empty),
        capture(Engine::Mysql, vec![], SourceState::Unavailable),
        capture(
            Engine::Postgres,
            vec![point(
                "pg_stat_archiver",
                NativeCounterSet {
                    ratios: Vec::new(),
                    ratio_gaps: Vec::new(),
                    identity: NativeIdentity::new("pg_stat_archiver"),
                    scope: MeasurementScope::Server,
                    counters: vec![counter(
                        "pg_stat_archiver",
                        "failed_count",
                        7,
                        CounterNature::Cumulative,
                        MeasurementScope::Server,
                        ResetMarker::Unknown,
                    )],
                    properties: BTreeMap::new(),
                },
            )],
            SourceState::Collected,
        ),
    ]
}

fn postgres_row(present: bool, version: u32, reset: ResetMarker) -> TelemetryRecord {
    let source = "pg_stat_database";
    let mut properties = BTreeMap::from([
        ("datname".into(), "application".into()),
        ("server_version_num".into(), version.to_string()),
        ("counter_observed_at".into(), "2020-01-02T12:00:00.500000Z".into()),
    ]);
    for key in [
        "sessions",
        "sessions_abandoned",
        "sessions_fatal",
        "sessions_killed",
        "session_time",
        "active_time",
        "idle_in_transaction_time",
    ] {
        properties.insert(format!("has_{key}"), present.to_string());
    }
    let mut counters =
        vec![counter(source, "xact_commit", 17, CounterNature::Cumulative, MeasurementScope::Database, reset.clone())];
    if present {
        counters.extend([
            counter(
                source,
                "sessions",
                9007199254740993,
                CounterNature::Cumulative,
                MeasurementScope::Database,
                reset.clone(),
            ),
            counter(
                source,
                "sessions_abandoned",
                0,
                CounterNature::Cumulative,
                MeasurementScope::Database,
                reset.clone(),
            ),
            counter(source, "sessions_killed", 5, CounterNature::Cumulative, MeasurementScope::Database, reset.clone()),
        ]);
        // sessions_fatal is a present native NULL; it remains no value instead of a counterfeit zero.
        for (key, value) in [("session_time", 123456.125), ("active_time", 0.0), ("idle_in_transaction_time", 12.5)] {
            counters.push(NativeCounterSample {
                key: key.into(),
                value: MeasurementValue::Real(value),
                unit: CounterUnit::Milliseconds,
                nature: CounterNature::Cumulative,
                scope: MeasurementScope::Database,
                reset: reset.clone(),
                native_object: source.into(),
                native_name: key.into(),
                native_instance: None,
                native_type: None,
            });
        }
    }
    point(
        source,
        NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new(source)
                .qualified("dbid", "16384")
                .qualified("postmaster_started_at", "2020-01-02T11:59:00Z"),
            scope: MeasurementScope::Database,
            counters,
            properties,
        },
    )
}

fn mysql_rows() -> Vec<TelemetryRecord> {
    [
        ("Connections", 18446744073709551615, CounterNature::Cumulative),
        ("Aborted_connects", 0, CounterNature::Cumulative),
        ("Aborted_clients", 9007199254740993, CounterNature::Cumulative),
        ("Connection_errors_accept", 5, CounterNature::Cumulative),
        ("Connection_errors_internal", 1, CounterNature::Cumulative),
        ("Connection_errors_max_connections", 3, CounterNature::Cumulative),
        ("Connection_errors_peer_address", 4, CounterNature::Cumulative),
        ("Connection_errors_select", 7, CounterNature::Cumulative),
        ("Connection_errors_tcpwrap", 0, CounterNature::Cumulative),
        ("Threads_created", 13, CounterNature::Cumulative),
        ("Threads_connected", 0, CounterNature::Gauge),
        ("Threads_running", 1, CounterNature::Gauge),
        // An unrelated variable shares the source/receipt but must not be removed or reinterpreted as an outcome.
        ("Innodb_buffer_pool_read_requests", 17, CounterNature::Cumulative),
    ]
    .into_iter()
    .map(|(key, value, nature)| {
        point(
            "global-status",
            NativeCounterSet {
                ratios: Vec::new(),
                ratio_gaps: Vec::new(),
                identity: NativeIdentity::new(key),
                scope: MeasurementScope::Server,
                counters: vec![counter(
                    "global-status",
                    key,
                    value,
                    nature,
                    MeasurementScope::Server,
                    ResetMarker::Unknown,
                )],
                properties: BTreeMap::new(),
            },
        )
    })
    .collect()
}

fn counter(
    source: &str,
    key: &str,
    value: u64,
    nature: CounterNature,
    scope: MeasurementScope,
    reset: ResetMarker,
) -> NativeCounterSample {
    NativeCounterSample {
        key: key.into(),
        value: MeasurementValue::Integer(value.to_string()),
        unit: CounterUnit::Count,
        nature,
        scope,
        reset,
        native_object: source.into(),
        native_name: key.into(),
        native_instance: None,
        native_type: None,
    }
}
fn point(source: &str, counters: NativeCounterSet) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation::from_counters(source, counters).unwrap())
}

fn capture(
    engine: Engine,
    records: Vec<TelemetryRecord>,
    state: SourceState,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut counts = BTreeMap::<String, u32>::new();
    for record in &records {
        let TelemetryRecord::Operational(row) = record else { unreachable!() };
        *counts.entry(row.source.clone()).or_default() += 1;
    }
    if counts.is_empty() {
        counts.insert(if engine == Engine::Postgres { "pg_stat_database" } else { "global-status" }.into(), 0);
    }
    let sources = counts
        .into_iter()
        .map(|(source, count)| {
            if state == SourceState::Unavailable {
                StateSourceCoverage::unavailable(source, culprit_core::ErrorCode::PermissionDenied, None)
            } else {
                StateSourceCoverage::observed(source, count, state != SourceState::Partial)
            }
        })
        .collect();
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            engine,
            "application",
            "fixture.invalid",
            if engine == Engine::Postgres { 5432 } else { 3306 },
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        source: "native-operational-outcome-fixture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-02T12:00:00Z".into(),
            observed_until: "2020-01-02T12:00:01Z".into(),
            records: records.len() as u32,
            sources,
        }),
    };
    assert!(source.partition_is_consistent());
    let rows = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { unreachable!() };
            row.clone()
        })
        .collect();
    assert!(ConnectionOutcomeSnapshot::from_source(&source, rows).unwrap().is_valid());
    (source, records)
}

pub fn assert_connection_outcome_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    let rows = capture
        .records()
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.row().record else {
                panic!("original native counter point")
            };
            row.clone()
        })
        .collect();
    let restored = ConnectionOutcomeSnapshot::from_source(capture.source(), rows).unwrap();
    assert!(restored.is_valid());
    assert_eq!(restored.target, source.target);
    assert_eq!(restored.sources, source.observation.snapshot().unwrap().sources);
    let ObservationReadContext::Native { context } = &restored.context else { panic!("original native descriptor") };
    assert_eq!(context.as_ref(), &NativeReadContext::from_snapshot(source).unwrap());
}
