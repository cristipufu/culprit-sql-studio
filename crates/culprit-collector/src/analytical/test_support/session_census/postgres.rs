//! Native-shaped PostgreSQL caller labels and generations, shared by canonical SDK/Outbox/store consumers.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, MeasurementScope};

pub(super) fn shared_labels() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let epoch = digest(b"postgres-database-process-native-generation");
    let source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Postgres,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("123456789").qualified("kind", "postgres-system-identifier"),
                native: NativeIdentity::new("127.0.0.1").qualified("port", "5432"),
            }),
            epoch: Some(epoch.clone()),
        },
        source: "pg_stat_activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: Some(true),
            epoch: Some(epoch),
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            at: "2020-01-01T12:00:00.5Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: 3,
            observations: 3,
            coverage: StateSourceCoverage::collected("pg_stat_activity".into(), 3),
        }),
    };
    let rows = [
        ("401", "2020-01-01T11:59:00.000001Z", Some("2020-01-01T11:59:59.000001Z")),
        ("402", "2020-01-01T11:59:00.000002Z", Some("2020-01-01T11:59:59.000002Z")),
        ("403", "2020-01-01T11:59:00.000003Z", None),
    ]
    .into_iter()
    .map(|(pid, backend, query)| {
        TelemetryRecord::from(ActivityObservation {
            key: ActivityKey {
                session: NativeActivityIdentity { id: pid.into(), generation: backend.into() },
                request: query.map(|start| NativeActivityIdentity { id: "statement".into(), generation: start.into() }),
                transaction: None,
            },
            idle: query.is_none(),
            scope: ActivityScopeBasis::SessionDatabase,
            state: if query.is_some() { "active" } else { "idle" }.into(),
            database: Some("db".into()),
            command: None,
            blocking_status: ActivityBlockingStatus::Unknown,
            last_request_age_ms: query.is_none().then(|| "250".into()),
            open_transactions: Some(u32::from(query.is_some())),
            elapsed_ms: query.map(|_| "10".into()),
            transaction_age_ms: None,
            session_counters: ActivityCounters::default(),
            request_counters: query.map(|_| ActivityCounters::default()),
            session_context: Some(Box::new(ActivitySessionContext {
                lifecycle: if query.is_some() { SessionLifecycle::Running } else { SessionLifecycle::Idle },
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: Some(Box::new(SessionNativeContext::Postgres(PostgresSessionContext {
                    database_oid: Some("42".into()),
                    database_name: Some("db".into()),
                    backend_type: Some("client backend".into()),
                }))),
            })),
            waiting_on_lock: query.map(|_| true),
            wait: query.map(|_| ActivityWait {
                class: None,
                identified_resources: Vec::new(),
                resources: None,
                native_type: Some("Lock: advisory".into()),
                elapsed_ms: None,
                owners: Vec::new(),
            }),
            query_hash: None,
            statement_identity: query
                .map(|query| culprit_core::engines::postgres_activity_statement_identity(pid, backend, query)),
            client: Some(ActivityClient {
                application: Some("fixture-shared-pooled-driver".into()),
                user: Some("shared-login".into()),
                host: Some("127.0.0.1".into()),
            }),
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        })
    })
    .collect::<Vec<_>>();
    let observations = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Activity(row) => row.as_ref().clone(),
            _ => unreachable!(),
        })
        .collect();
    assert!(ActivityCensusRead::new(source.clone(), observations, true).unwrap().is_valid());
    (source, rows)
}
