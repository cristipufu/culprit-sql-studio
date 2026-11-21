//! Native-shaped census facts without a live/native-cost or current-owner qualification claim.
use super::*;
mod groups;
mod mysql;
mod postgres;

#[derive(Clone, Copy, Debug)]
pub enum SessionCensusCase {
    Idle,
    Dormant,
    Mars,
    Unknown,
    Partial,
    Unavailable,
    Empty,
    PostgresSharedLabels,
    GroupedClients,
    ContradictoryMars,
    GroupUnknownTotals,
    NativeSignedElapsed,
    IdleConnection,
    IdleConnectionMissing,
    IdleConnectionScopeUnavailable,
    IdleConnectionAmbiguous,
    IdleConnectionVisibilityUnknown,
    IdleConnectionVisibilityDenied,
    PostgresWorker,
    MysqlShared,
    MysqlSwitched,
    MysqlWorker,
}
impl SessionCensusCase {
    pub const ALL: [Self; 22] = [
        Self::Idle,
        Self::Dormant,
        Self::Mars,
        Self::Unknown,
        Self::Partial,
        Self::Unavailable,
        Self::Empty,
        Self::PostgresSharedLabels,
        Self::GroupedClients,
        Self::ContradictoryMars,
        Self::GroupUnknownTotals,
        Self::NativeSignedElapsed,
        Self::IdleConnection,
        Self::IdleConnectionMissing,
        Self::IdleConnectionScopeUnavailable,
        Self::IdleConnectionAmbiguous,
        Self::IdleConnectionVisibilityUnknown,
        Self::IdleConnectionVisibilityDenied,
        Self::PostgresWorker,
        Self::MysqlShared,
        Self::MysqlSwitched,
        Self::MysqlWorker,
    ];
}

pub fn session_census_evidence(case: SessionCensusCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    if matches!(case, SessionCensusCase::PostgresSharedLabels | SessionCensusCase::PostgresWorker) {
        let (mut source, mut rows) = postgres::shared_labels();
        if matches!(case, SessionCensusCase::PostgresWorker) {
            let TelemetryRecord::Activity(row) = &mut rows[0] else { unreachable!() };
            let Some(SessionNativeContext::Postgres(native)) =
                row.session_context.as_deref_mut().unwrap().native.as_deref_mut()
            else {
                unreachable!()
            };
            native.backend_type = Some("parallel worker".into());
            let SourceObservation::Activity(header) = &mut source.observation else { unreachable!() };
            header.population = ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers;
        }
        return (source, rows);
    }
    if matches!(
        case,
        SessionCensusCase::MysqlShared | SessionCensusCase::MysqlSwitched | SessionCensusCase::MysqlWorker
    ) {
        return mysql::evidence(case);
    }
    let mut read: ActivityCensusRead = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/session-census.native.json"
    )))
    .unwrap();
    let SessionNativeContext::Mssql(native) =
        read.observations[0].session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap()
    else {
        panic!("MSSQL test context")
    };
    native.session_database_id = Some("7".into());
    native.session_database_name = Some("db".into());
    native.request_database_id = None;
    native.elapsed_ms = Some("600000".into());
    native.idle_connection = None;
    match case {
        SessionCensusCase::Idle | SessionCensusCase::Partial => (),
        SessionCensusCase::IdleConnection => {
            native.idle_connection = Some(MssqlIdleSessionConnection::Observed {
                connection_id: "37dd5a75-5bb6-4487-89a7-a6b1b0d1ca12".into(),
                connected_without_offset: "2019-12-31T23:00:00.0000000".into(),
            })
        }
        SessionCensusCase::IdleConnectionScopeUnavailable => {
            native.idle_connection = Some(MssqlIdleSessionConnection::ScopeUnavailable)
        }
        SessionCensusCase::IdleConnectionMissing => native.idle_connection = Some(MssqlIdleSessionConnection::Missing),
        SessionCensusCase::IdleConnectionAmbiguous => {
            native.idle_connection = Some(MssqlIdleSessionConnection::Ambiguous)
        }
        SessionCensusCase::IdleConnectionVisibilityUnknown => {
            native.idle_connection = Some(MssqlIdleSessionConnection::VisibilityUnavailable { permitted: None })
        }
        SessionCensusCase::IdleConnectionVisibilityDenied => {
            native.idle_connection = Some(MssqlIdleSessionConnection::VisibilityUnavailable { permitted: Some(false) })
        }
        SessionCensusCase::Dormant => {
            let row = &mut read.observations[0];
            row.key.session.generation = "1900-01-01T00:00:00.0000000".into();
            row.state = "dormant".into();
            row.last_request_age_ms = None;
            let context = row.session_context.as_deref_mut().unwrap();
            context.lifecycle = SessionLifecycle::PoolResetPrelogin;
            context.login_age_ms = None;
            context.last_request_end_age_ms = None;
            let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap() else {
                panic!("MSSQL test context")
            };
            native.native_state = "Dormant".into();
            native.session_database_id = Some("0".into());
            native.session_database_name = None;
            native.login_established = false;
            native.elapsed_ms = None;
            native.times.login_without_offset = row.key.session.generation.clone();
            native.times.last_request_start_without_offset = None;
            native.times.last_request_end_without_offset = None;
        }
        SessionCensusCase::Mars => {
            let first = current_request(read.observations.remove(0), "0", "11111111-1111-1111-1111-111111111111");
            let second = current_request(first.clone(), "1", "22222222-2222-2222-2222-222222222222");
            read.observations = vec![first, second];
        }
        SessionCensusCase::Unknown => {
            let row = &mut read.observations[0];
            row.last_request_age_ms = None;
            row.open_transactions = None;
            row.session_counters = ActivityCounters::default();
            let context = row.session_context.as_deref_mut().unwrap();
            context.lifecycle = SessionLifecycle::Unknown;
            context.login_age_ms = None;
            context.last_request_end_age_ms = None;
            context.client_library = None;
            context.client_process_id = None;
            let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap() else {
                panic!("MSSQL test context")
            };
            native.native_state = "future-native-state".into();
            native.elapsed_ms = None;
            native.session_database_id = None;
            native.session_database_name = None;
            native.times.last_request_start_without_offset = None;
            native.times.last_request_end_without_offset = None;
        }
        SessionCensusCase::Unavailable | SessionCensusCase::Empty => read.observations.clear(),
        SessionCensusCase::GroupedClients
        | SessionCensusCase::ContradictoryMars
        | SessionCensusCase::GroupUnknownTotals
        | SessionCensusCase::NativeSignedElapsed => {
            groups::apply(&mut read, case);
        }
        SessionCensusCase::PostgresSharedLabels
        | SessionCensusCase::PostgresWorker
        | SessionCensusCase::MysqlShared
        | SessionCensusCase::MysqlSwitched
        | SessionCensusCase::MysqlWorker => unreachable!(),
    }
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.observations = read.observations.len() as u32;
    header.native_rows = header.observations;
    header.coverage = StateSourceCoverage::collected(culprit_core::source_id::SESSIONS.into(), header.observations);
    match case {
        SessionCensusCase::Partial => {
            header.native_rows += 1;
            header.coverage =
                StateSourceCoverage::observed(culprit_core::source_id::SESSIONS.into(), header.observations, false);
            header.coverage.stopped = Some(CollectionStop::RowLimit);
        }
        SessionCensusCase::Unavailable => {
            header.epoch = None;
            header.coverage = StateSourceCoverage::unavailable(
                culprit_core::source_id::SESSIONS.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            );
        }
        _ => (),
    }
    assert!(read.is_valid(), "{case:?}");
    (read.source, read.observations.into_iter().map(TelemetryRecord::from).collect())
}

fn current_request(mut row: ActivityObservation, id: &str, connection: &str) -> ActivityObservation {
    row.key.request = Some(NativeActivityIdentity { id: id.into(), generation: "2020-01-01T11:59:59.0000000".into() });
    row.idle = false;
    row.scope = ActivityScopeBasis::RequestDatabase;
    row.state = "running".into();
    row.command = Some("SELECT".into());
    row.elapsed_ms = Some("10".into());
    row.request_counters = Some(ActivityCounters { cpu_ms: Some("0".into()), ..Default::default() });
    let context = row.session_context.as_deref_mut().unwrap();
    context.lifecycle = SessionLifecycle::Running;
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap() else {
        panic!("MSSQL test context")
    };
    native.native_state = "Running".into();
    native.session_database_id = Some("4".into());
    native.session_database_name = Some("session-default".into());
    native.request_database_id = Some("7".into());
    native.has_current_request = true;
    native.request_connection_id = Some(connection.into());
    native.idle_connection = None;
    row
}
