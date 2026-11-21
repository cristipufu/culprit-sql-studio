//! Native-shaped current-default-schema metadata; synthetic contract controls, not native acceptance.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, MeasurementScope};

pub(super) fn evidence(case: SessionCensusCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let switched = matches!(case, SessionCensusCase::MysqlSwitched);
    let database = if switched { "other" } else { "db" };
    let mut source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mysql, database, "fixture.invalid", 3306, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mysql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("mysql-server-uuid").qualified("kind", "mysql-server-uuid"),
                native: NativeIdentity::new("f54a461c-0001-4000-8000-000000000001"),
            }),
            epoch: Some(digest(b"original-mysql-process-generation")),
        },
        source: "mysql-current-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: Some(true),
            epoch: Some(digest(b"original-mysql-process-generation")),
            observed_from: if switched { "2020-01-01T12:00:01Z" } else { "2020-01-01T12:00:00Z" }.into(),
            observed_until: if switched { "2020-01-01T12:00:02Z" } else { "2020-01-01T12:00:01Z" }.into(),
            at: if switched { "2020-01-01T12:00:01.5Z" } else { "2020-01-01T12:00:00.5Z" }.into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::DefaultDatabaseConnections,
            native_rows: 2,
            observations: 2,
            coverage: StateSourceCoverage::collected("mysql-current-activity".into(), 2),
        }),
    };
    let mut rows = [false, true]
        .into_iter()
        .enumerate()
        .map(|(index, current)| {
            let request_schema = current.then(|| if switched { "db" } else { "statement-default" }.to_owned());
            let session =
                NativeActivityIdentity { id: (701 + index).to_string(), generation: (1001 + index).to_string() };
            let request = current.then(|| NativeActivityIdentity { id: "3".into(), generation: "4".into() });
            TelemetryRecord::from(ActivityObservation {
                session_context: Some(Box::new(ActivitySessionContext {
                    lifecycle: if current { SessionLifecycle::Running } else { SessionLifecycle::Idle },
                    login_age_ms: None,
                    last_request_end_age_ms: None,
                    client_library: None,
                    client_process_id: None,
                    native: Some(Box::new(SessionNativeContext::Mysql(MysqlSessionContext {
                        default_database: Some(database.into()),
                        request_schema: request_schema.clone(),
                        thread_type: Some("FOREGROUND".into()),
                        thread_name: Some("thread/sql/one_connection".into()),
                    }))),
                })),
                blocking_status: ActivityBlockingStatus::Unknown,
                waiting_on_lock: None,
                key: ActivityKey { session, request, transaction: None },
                idle: !current,
                scope: ActivityScopeBasis::SessionDatabase,
                state: if current { "executing" } else { "Sleep" }.into(),
                database: request_schema.or_else(|| Some(database.into())),
                command: Some(if current { "Query" } else { "Sleep" }.into()),
                last_request_age_ms: None,
                open_transactions: None,
                elapsed_ms: current.then(|| "10".into()),
                transaction_age_ms: None,
                session_counters: ActivityCounters::default(),
                request_counters: current.then(ActivityCounters::default),
                wait: None,
                query_hash: None,
                statement_identity: None,
                client: Some(ActivityClient {
                    application: None,
                    user: Some("shared-login".into()),
                    host: Some("shared-client".into()),
                }),
                statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            })
        })
        .collect::<Vec<_>>();
    if matches!(case, SessionCensusCase::MysqlWorker) {
        let TelemetryRecord::Activity(row) = &mut rows[1] else { unreachable!() };
        let Some(SessionNativeContext::Mysql(native)) =
            row.session_context.as_deref_mut().unwrap().native.as_deref_mut()
        else {
            unreachable!()
        };
        native.thread_type = Some("BACKGROUND".into());
        native.thread_name = Some("thread/sql/slave_sql".into());
        let SourceObservation::Activity(header) = &mut source.observation else { unreachable!() };
        header.population = ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers;
        header.coverage = StateSourceCoverage::observed("mysql-current-activity".into(), 2, false);
        header.coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    (source, rows)
}
