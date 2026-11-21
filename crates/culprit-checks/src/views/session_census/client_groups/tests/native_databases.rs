use super::*;
use culprit_core::{Engine, SessionClientDatabase};

fn source(mut read: ActivityCensusRead, engine: Engine, population: ActivityPopulation) -> ActivityCensusRead {
    let CollectionTarget::Database(target) = &mut read.source.target else { panic!() };
    target.engine = engine;
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.population = population;
    read
}
fn postgres_read() -> ActivityCensusRead {
    let mut read = source(read(), Engine::Postgres, ActivityPopulation::ClientBackends);
    let row = &mut read.observations[0];
    row.key.session.generation = "2020-01-01T11:50:00.000001Z".into();
    row.session_counters = ActivityCounters::default();
    row.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::Idle,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: Some(Box::new(SessionNativeContext::Postgres(PostgresSessionContext {
            database_oid: Some("42".into()),
            database_name: Some("backend-db".into()),
            backend_type: Some("client backend".into()),
        }))),
    }));
    read
}
fn mysql_read() -> ActivityCensusRead {
    let mut read = source(read(), Engine::Mysql, ActivityPopulation::DefaultDatabaseConnections);
    let row = &mut read.observations[0];
    row.key.session.generation = "1001".into();
    row.key.request = Some(NativeActivityIdentity { id: "3".into(), generation: "4".into() });
    row.request_counters = Some(ActivityCounters::default());
    row.idle = false;
    row.command = Some("Query".into());
    row.scope = ActivityScopeBasis::SessionDatabase;
    row.database = Some("statement-default".into());
    row.session_counters = ActivityCounters::default();
    row.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::Running,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: Some(Box::new(SessionNativeContext::Mysql(MysqlSessionContext {
            default_database: Some("session-default".into()),
            request_schema: Some("statement-default".into()),
            thread_type: Some("FOREGROUND".into()),
            thread_name: Some("thread/sql/one_connection".into()),
        }))),
    }));
    read
}
fn postgres(row: &mut ActivityObservation) -> &mut PostgresSessionContext {
    let Some(SessionNativeContext::Postgres(native)) =
        row.session_context.as_deref_mut().unwrap().native.as_deref_mut()
    else {
        panic!()
    };
    native
}
fn mysql(row: &mut ActivityObservation) -> &mut MysqlSessionContext {
    let Some(SessionNativeContext::Mysql(native)) = row.session_context.as_deref_mut().unwrap().native.as_deref_mut()
    else {
        panic!()
    };
    native
}

#[test]
fn native_backend_database_and_utc_start_group_without_reusing_request_time_or_mssql_totals() {
    let mut read = postgres_read();
    read.observations[0].idle = false;
    read.observations[0].state = "active".into();
    read.observations[0].key.request =
        Some(NativeActivityIdentity { id: "statement".into(), generation: "2020-01-01T11:59:59.000001Z".into() });
    read.observations[0].request_counters =
        Some(ActivityCounters { logical_reads: Some("9007199254740993".into()), ..Default::default() });
    read.observations[0].session_context.as_deref_mut().unwrap().lifecycle = SessionLifecycle::Running;
    read.observations[0].session_context.as_deref_mut().unwrap().login_age_ms = Some("600000".into());
    let mut peer = read.observations[0].clone();
    peer.key.session.id = "99".into();
    peer.key.session.generation = "2020-01-01T11:55:00.000001Z".into();
    peer.session_context.as_deref_mut().unwrap().login_age_ms = Some("300000".into());
    let original = read.observations[0].clone();
    let analysis = report(&with_rows(read, vec![original, peer]));
    let group = &analysis.client_groups.entries[0];
    assert_eq!(group.observed_sessions, 2);
    assert_eq!(
        group.key.database,
        SessionClientDatabase::Backend { native_id: "42".into(), name: Some("backend-db".into()) }
    );
    assert_eq!(value(group, "earliest_backend_start_utc"), Some("2020-01-01T11:50:00.000001Z"));
    for field in ["session_age_ms", "session_logical_reads", "session_writes"] {
        assert_eq!(value(group, field), None);
    }
    assert!(group.limitations.contains(&t!("view.session_census.groups.limit.backend")));
}

#[test]
fn native_worker_kinds_do_not_gain_client_counts_from_identical_labels_or_active_state() {
    for kind in [
        None,
        Some("parallel worker"),
        Some("autovacuum worker"),
        Some("yb-conn-mgr worker connection"),
        Some("future native kind"),
    ] {
        let mut read = postgres_read();
        postgres(&mut read.observations[0]).backend_type = kind.map(str::to_owned);
        let analysis = report(&read);
        assert_eq!((analysis.client_groups.grouped_sessions, analysis.client_groups.ungrouped_sessions), (0, 1));
        assert_eq!(analysis.entries.len(), 1);
    }
    for (kind, name) in [
        (Some("BACKGROUND"), Some("thread/sql/one_connection")),
        (Some("FOREGROUND"), Some("thread/sql/slave_sql")),
        (Some("FOREGROUND"), None),
    ] {
        let mut read = mysql_read();
        let native = mysql(&mut read.observations[0]);
        native.thread_type = kind.map(str::to_owned);
        native.thread_name = name.map(str::to_owned);
        let analysis = report(&read);
        assert_eq!((analysis.client_groups.grouped_sessions, analysis.client_groups.ungrouped_sessions), (0, 1));
        assert_eq!(analysis.entries.len(), 1);
    }
}

#[test]
fn mysql_current_default_schema_never_uses_request_schema_as_id_or_connection_age() {
    let read = mysql_read();
    let analysis = report(&read);
    let group = &analysis.client_groups.entries[0];
    assert_eq!((group.observed_sessions, group.observed_requests), (1, 1));
    assert_eq!(group.key.database, SessionClientDatabase::CurrentDefault { name: "session-default".into() });
    assert_eq!(value(group, "session_database_id"), None);
    assert_eq!(value(group, "session_database_name"), Some("session-default"));
    for field in ["session_age_ms", "session_logical_reads", "session_writes"] {
        assert_eq!(value(group, field), None);
    }
    assert!(group.limitations.contains(&t!("view.session_census.groups.limit.default_schema")));
    let row = &analysis.entries[0];
    assert_eq!(
        row.fields.iter().find(|field| field.key == "current_statement_schema").unwrap().value.as_deref(),
        Some("statement-default")
    );
}

#[test]
fn switching_default_schema_preserves_each_original_without_merging_by_native_thread_or_labels() {
    let read = mysql_read();
    let before = serde_json::to_value(report(&read)).unwrap();
    let mut later = read.clone();
    mysql(&mut later.observations[0]).default_database = Some("changed".into());
    let analysis = report(&later);
    assert_eq!(
        analysis.client_groups.entries[0].key.database,
        SessionClientDatabase::CurrentDefault { name: "changed".into() }
    );
    assert_eq!(serde_json::to_value(report(&read)).unwrap(), before);
    mysql(&mut later.observations[0]).default_database = None;
    let unknown = report(&later);
    assert_eq!((unknown.client_groups.grouped_sessions, unknown.client_groups.ungrouped_sessions), (0, 1));
    assert_eq!(unknown.entries.len(), 1);
}

#[test]
fn a_client_class_is_not_a_business_owner_and_labels_cannot_repair_an_unknown_class() {
    let mut read = mysql_read();
    read.observations[0].client.as_mut().unwrap().user = Some("system user".into());
    assert_eq!(
        report(&read).client_groups.grouped_sessions,
        1,
        "username is a label, not native client-class authority"
    );
    mysql(&mut read.observations[0]).thread_name = Some("unknown native instrument".into());
    read.observations[0].client.as_mut().unwrap().application = Some("one_connection".into());
    let analysis = report(&read);
    assert_eq!((analysis.client_groups.grouped_sessions, analysis.client_groups.ungrouped_sessions), (0, 1));
}
