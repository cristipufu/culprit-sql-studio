//! The same native diagnostics exercise SDK consumers and both physical warehouse adapters.
use super::*;

pub fn native_failure_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    vec![mysql(), postgres()]
}

fn mysql() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let rows = [Some(u64::MAX.to_string()), Some(u64::MAX.to_string()), Some("0".into()), None]
        .into_iter()
        .enumerate()
        .map(|(position, thread_id)| {
            NativeFailureRecord::MysqlErrorLog(MysqlErrorLogObservation {
                position: position as u32,
                logged_at: "2020-01-01T12:00:00.123456Z".into(),
                thread_id,
                priority: "Error".into(),
                error_code: if position == 3 { None } else { Some("MY-010935".into()) },
                subsystem: if position == 3 { None } else { Some("Server".into()) },
                message: None,
            })
        })
        .collect();
    capture(culprit_core::Engine::Mysql, "native-error-buffer", rows, true)
}

fn postgres() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let statement = DefinitionPayload::sql(
        "UPDATE app.items SET payload='private-pg-log-canary' WHERE item_id=9264",
        culprit_core::Engine::Postgres,
    );
    let allocation = PostgresLogFailureObservation {
        position: 0,
        timestamp: "2020-01-01 10:00:01.123 EET".into(),
        timestamp_utc: None,
        sql_state: "53200".into(),
        severity: Some("ERROR".into()),
        database: Some("affected-app".into()),
        process_id: Some("539".into()),
        session_id: Some("6915c000.21b".into()),
        session_start: Some("2020-01-01 09:30:00 EET".into()),
        session_line_number: Some("7".into()),
        virtual_transaction_id: Some("4/42".into()),
        transaction_id: Some("838".into()),
        query_id: Some(i64::MIN.to_string()),
        leader_process_id: Some("0".into()),
        backend_type: Some("client backend".into()),
        statement,
        deadlock: None,
    };
    assert!(allocation.is_valid());
    let mut deadlock = allocation.clone();
    deadlock.position = 1;
    deadlock.sql_state = "40P01".into();
    let DefinitionPayload::Available { text, .. } = &deadlock.statement else { unreachable!() };
    deadlock.deadlock = Some(PostgresLogDeadlock {
        processes: vec![
            PostgresLogDeadlockProcess {
                process_id: "539".into(),
                victim: true,
                wait_mode: Some("ShareLock".into()),
                statement: Some(text.clone()),
            },
            PostgresLogDeadlockProcess {
                process_id: "545".into(),
                victim: false,
                wait_mode: Some("ShareLock".into()),
                statement: None,
            },
        ],
        resources: vec![
            PostgresLogDeadlockResource {
                native_id: "transaction 838".into(),
                kind: "transaction".into(),
                relation: None,
                holder: "545".into(),
                waiter: "539".into(),
                wait_mode: "ShareLock".into(),
            },
            PostgresLogDeadlockResource {
                native_id: "transaction 837".into(),
                kind: "transaction".into(),
                relation: None,
                holder: "539".into(),
                waiter: "545".into(),
                wait_mode: "ShareLock".into(),
            },
        ],
        pattern: culprit_core::DeadlockPattern::OppositeOrder,
    });
    assert!(deadlock.is_valid());
    capture(
        culprit_core::Engine::Postgres,
        "native-structured-feed",
        vec![
            NativeFailureRecord::PostgresLog(Box::new(allocation)),
            NativeFailureRecord::PostgresLog(Box::new(deadlock)),
        ],
        false,
    )
}

fn capture(
    engine: culprit_core::Engine,
    native_source: &str,
    native: Vec<NativeFailureRecord>,
    complete_scope: bool,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    manifest.source.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("fixture-native-server"),
        native: NativeIdentity::new("fixture-native-instance").qualified("native_endpoint", "fixture.invalid:5432"),
    });
    manifest.source.source = "native-diagnostic-capture".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::NativeFailures,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: native.len() as u32,
        sources: vec![StateSourceCoverage::observed(native_source.into(), native.len() as u32, complete_scope)],
    });
    let records = native
        .into_iter()
        .map(|row| {
            let (key, fact) = match row {
                NativeFailureRecord::MysqlErrorLog(row) => {
                    (row.key().unwrap(), OperationalFact::MysqlErrorLog(Box::new(row)))
                }
                NativeFailureRecord::PostgresLog(row) => (row.key().unwrap(), OperationalFact::PostgresLog(row)),
            };
            TelemetryRecord::Operational(OperationalObservation { source: native_source.into(), key, fact })
        })
        .collect();
    (manifest.source, records)
}
