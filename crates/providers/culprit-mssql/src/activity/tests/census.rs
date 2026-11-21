use super::*;

fn census_row(id: i64, request: Option<i64>, native_state: &str) -> Rec {
    let base = metadata(id);
    let mut columns = base.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    columns.extend([
        ("session_context_collected", Val::Int(1)),
        ("session_database_id", Val::Int(7)),
        ("session_database_name", Val::Text("shop".into())),
        ("native_session_state", Val::Text(native_state.into())),
        ("client_interface_name", Val::Text("owned driver library".into())),
        ("host_process_id", Val::Int(0)),
        ("client_version", Val::Null),
        ("session_elapsed_ms", Val::Int(123456)),
        (
            "request_connection_id",
            if request.is_some() { Val::Text("a8a44410-5033-4db1-9e90-79354ab31702".into()) } else { Val::Null },
        ),
    ]);
    let row = Rec::of(&columns);
    if let Some(request) = request {
        return replace(&row, &[("request_id", Val::Int(request))]);
    }
    replace(
        &row,
        &[
            ("request_id", Val::Null),
            ("request_database_id", Val::Null),
            ("request_generation", Val::Null),
            ("no_current_request", Val::Int(1)),
            ("scope_basis", Val::Int(3)),
            ("state", Val::Text("sleeping".into())),
            ("command", Val::Null),
            ("open_transaction_count", Val::Int(0)),
            ("transaction_id", Val::Null),
            ("transaction_start", Val::Null),
            ("elapsed_ms", Val::Null),
            ("wait_type", Val::Null),
            ("wait_ms", Val::Null),
            ("blocking_session_id", Val::Null),
            ("query_hash", Val::Null),
            ("sql_handle", Val::Null),
            ("statement_start", Val::Null),
            ("statement_end", Val::Null),
        ],
    )
}

#[tokio::test]
async fn quiet_user_sessions_keep_library_lifetimes_and_exact_counters_without_a_sql_text_read() {
    let mut reader = reader(vec![Ok(vec![census_row(55, None, "Sleeping")])], IdentityDetail::Endpoint);
    let live = crate::live::collect_from(&mut reader).await.unwrap();
    assert_eq!(reader.query_count(), 1);
    assert_eq!(reader.io.closed, 1);
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("sys.dm_exec_sql_text")));
    let session = &live.sessions[0];
    assert!(session.sql.is_none() && session.elapsed_ms.is_none() && session.sql_context.is_none());
    let read = live.activity_census.unwrap();
    assert!(read.is_valid());
    let point = &read.observations[0];
    assert!(point.idle && point.conditions(&ActivityPolicy::default()).is_empty());
    assert_eq!(point.session_counters.logical_reads.as_deref(), Some("9007199254740993"));
    let context = point.session_context.as_ref().unwrap();
    assert_eq!(context.lifecycle, SessionLifecycle::Idle);
    assert_eq!(context.client_library.as_deref(), Some("owned driver library"));
    assert_eq!(context.client_process_id.as_deref(), Some("0"));
    assert!(context.login_age_ms.is_some());
    let SessionNativeContext::Mssql(native) = context.native.as_deref().unwrap() else { panic!("MSSQL test context") };
    assert!(native.tds_protocol_version.is_none() && native.request_connection_id.is_none());
    assert_eq!(native.elapsed_ms.as_deref(), Some("123456"));
    assert_ne!(native.elapsed_ms, context.login_age_ms);
    assert!(reader.io.calls[0].sql.contains("s.total_elapsed_time AS session_elapsed_ms"));
    assert_eq!(native.session_database_id.as_deref(), Some("7"));
    assert_eq!(native.session_database_name.as_deref(), Some("shop"));
    assert!(native.request_database_id.is_none());
    assert_eq!(native.times.last_request_start_without_offset.as_deref(), Some("2020-01-01T07:55:00.0000000"));
}

#[tokio::test]
async fn quiet_rows_do_not_expand_the_existing_active_request_sql_enrichment_population() {
    // metadata(42) normally names session55 as its blocker. This control is an unrelated quiet row,
    // so it must not enter the already supported blocker/prior-batch enrichment population.
    let active = replace(&census_row(42, Some(0), "Running"), &[("blocking_session_id", Val::Int(0))]);
    let mut reader = reader(
        vec![Ok(vec![active, census_row(55, None, "Sleeping")]), Ok(vec![details(42, "SELECT 1")])],
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut reader).await.unwrap();
    assert_eq!(live.sessions.len(), 2);
    assert_eq!(reader.query_count(), 2);
    assert_eq!(reader.io.closed, 1);
    assert_eq!(reader.io.calls[1].sql, sql::details(false, sql::QueryPolicy::Activity));
    assert!(!xml_parameter(&reader.io.calls[1]).contains("session=\"55\""));
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("most_recent_sql_handle")));
    assert_eq!(
        live.sessions.iter().find(|session| session.id == "42").unwrap().sql_context,
        Some(culprit_core::SessionStatementContext::CurrentRequest)
    );
}

#[tokio::test]
async fn mars_retains_each_exact_current_request_connection_without_a_representative_session_row() {
    let a = replace(
        &census_row(42, Some(0), "Running"),
        &[("session_database_id", Val::Int(4)), ("session_database_name", Val::Text("session-default".into()))],
    );
    let b = replace(
        &a,
        &[
            ("request_id", Val::Int(1)),
            ("request_connection_id", Val::Text("b8a44410-5033-4db1-9e90-79354ab31702".into())),
        ],
    );
    let mut reader = baseline_reader(vec![a, b]);
    let (source, rows) = crate::telemetry::activity::capture(&mut reader).await.unwrap();
    assert_eq!(reader.query_count(), 3);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key.session, rows[1].key.session);
    assert_ne!(rows[0].key.request, rows[1].key.request);
    let ids = rows
        .iter()
        .map(|row| {
            let SessionNativeContext::Mssql(native) = row.session_context.as_ref().unwrap().native.as_deref().unwrap()
            else {
                panic!("MSSQL test context")
            };
            assert_eq!(native.session_database_id.as_deref(), Some("4"));
            assert_eq!(native.session_database_name.as_deref(), Some("session-default"));
            assert_eq!(native.request_database_id.as_deref(), Some("7"));
            native.request_connection_id.as_deref().unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 2);
    assert!(ActivityCensusRead::new(source, rows, true).unwrap().is_valid());
}

#[tokio::test]
async fn prelogin_sentinels_and_malformed_details_cannot_authorize_current_request_work() {
    let row =
        replace(&census_row(55, None, "Dormant"), &[("session_generation", Val::Text("1900-01-01T00:00:00".into()))]);
    let mut reader = reader(vec![Ok(vec![row])], IdentityDetail::Endpoint);
    let frame = reader.observe_baseline().await.and_then(crate::activity::ActivityBaseline::into_complete).unwrap();
    let point = &frame.records[0];
    assert!(!point.can_identify_session());
    assert!(frame.sql_context_keys().is_empty());
    assert!(point.facts.conditions(&ActivityPolicy::default()).is_empty());
    assert!(point.facts.session_context.as_ref().unwrap().login_age_ms.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    for (column, value) in [
        ("client_version", Val::Float(1.5)),
        ("request_connection_id", Val::Text("foreign-not-native-guid".into())),
        ("session_context_collected", Val::Null),
    ] {
        let row = replace(&census_row(42, Some(0), "Running"), &[(column, value)]);
        let mut reader = super::reader(vec![Ok(vec![row])], IdentityDetail::Endpoint);
        assert_eq!(
            CollectionStop::from_error(
                &reader
                    .observe_baseline()
                    .await
                    .and_then(crate::activity::ActivityBaseline::into_complete)
                    .err()
                    .unwrap()
            ),
            CollectionStop::InvalidPage
        );
        assert_eq!(reader.query_count(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn limits_keep_request_priority_and_explicit_original_population_gaps() {
    let mut reader = baseline_reader(vec![census_row(42, Some(0), "Running"), census_row(55, None, "Sleeping")]);
    reader.options.budget.max_snapshot_rows = 1;
    let (source, rows) = crate::telemetry::activity::capture(&mut reader).await.unwrap();
    let native = source.observation.activity().unwrap();
    assert_eq!(native.native_rows, 2);
    assert_eq!(native.observations, 1);
    assert!(!native.coverage.complete_scope);
    assert_eq!(native.coverage.stopped, Some(CollectionStop::RowLimit));
    assert_eq!(rows[0].key.session.id, "42");
    let query = &reader.io.calls[1].sql;
    assert!(query.contains("OR s.database_id=DB_ID()"));
    assert!(query.contains("CASE WHEN a.request_id IS NOT NULL THEN 0"));
    assert!(!query.contains("sys.dm_exec_sql_text"));
    assert!(sql::details(true, sql::QueryPolicy::Activity).contains("c.most_recent_session_id=s.session_id"));
}

#[test]
fn current_request_database_context_survives_an_unavailable_sql_locator_and_different_session_database() {
    for (request_database, expected) in [(Val::Int(7), Some("7")), (Val::Int(0), Some("0")), (Val::Null, None)] {
        let row = replace(
            &census_row(42, Some(0), "Running"),
            &[
                ("session_database_id", Val::Int(4)),
                ("session_database_name", Val::Text("session-default".into())),
                ("request_database_id", request_database),
                ("sql_handle", Val::Null),
            ],
        );
        let record = rows::record(&row).unwrap().unwrap();
        assert!(record.current_locator.is_none());
        assert!(record.facts.statement_identity.is_none());
        let SessionNativeContext::Mssql(native) =
            record.facts.session_context.as_ref().unwrap().native.as_deref().unwrap()
        else {
            panic!("MSSQL test context")
        };
        assert_eq!(native.session_database_id.as_deref(), Some("4"));
        assert_eq!(native.session_database_name.as_deref(), Some("session-default"));
        assert_eq!(native.request_database_id.as_deref(), expected);
    }
    let sql = sql::metadata(IdentityDetail::Endpoint, page_server_reads::Availability::Unknown);
    assert!(sql.contains("s.database_id AS session_database_id"));
    assert!(sql.contains("CASE WHEN s.database_id>0 THEN DB_NAME(s.database_id) END AS session_database_name"));
    assert!(sql.contains("r.database_id AS request_database_id"));
}

#[test]
fn null_zero_and_hidden_native_session_database_names_are_retained_without_request_or_target_fallback() {
    for (id, expected) in [(Val::Null, None), (Val::Int(0), Some("0")), (Val::Int(7), Some("7"))] {
        let row = replace(
            &census_row(55, None, "Sleeping"),
            &[("session_database_id", id), ("session_database_name", Val::Null)],
        );
        let record = rows::record(&row).unwrap().unwrap();
        let SessionNativeContext::Mssql(native) =
            record.facts.session_context.as_ref().unwrap().native.as_deref().unwrap()
        else {
            panic!("MSSQL test context")
        };
        assert_eq!(native.session_database_id.as_deref(), expected);
        assert!(native.session_database_name.is_none());
        assert!(native.request_database_id.is_none());
        assert!(record.current_locator.is_none());
        assert_eq!(
            record.facts.database,
            Some("shop".into()),
            "existing display context cannot fill an independent native field"
        );
    }
}

#[test]
fn malformed_or_incoherent_native_database_metadata_is_rejected_by_the_same_decoder() {
    for changes in [
        vec![("session_database_id", Val::Int(-1))],
        vec![("session_database_id", Val::Int(32768))],
        vec![("session_database_id", Val::Float(7.5))],
        vec![("session_database_id", Val::Null)],
        vec![("session_database_id", Val::Int(0))],
        vec![("request_database_id", Val::Int(7))],
    ] {
        let row = replace(&census_row(55, None, "Sleeping"), &changes);
        assert!(rows::record(&row).is_err());
    }
}

#[tokio::test]
async fn same_session_projection_preserves_signed_native_elapsed_zero_unknown_and_range_without_an_extra_query() {
    for (value, expected) in [
        (Val::Null, None),
        (Val::Int(0), Some("0")),
        (Val::Int(i32::MAX.into()), Some("2147483647")),
        (Val::Int(-1), Some("-1")),
        (Val::Int(i32::MIN.into()), Some("-2147483648")),
    ] {
        let row = replace(&census_row(55, None, "Sleeping"), &[("session_elapsed_ms", value)]);
        let mut reader = reader(vec![Ok(vec![row])], IdentityDetail::Endpoint);
        let result = crate::live::collect_from(&mut reader).await;
        assert_eq!(reader.io.closed, 1);
        let live = result.unwrap();
        let context = live.activity_census.as_ref().unwrap().observations[0].session_context.as_ref().unwrap();
        let SessionNativeContext::Mssql(native) = context.native.as_deref().unwrap() else {
            panic!("MSSQL test context")
        };
        assert_eq!(native.elapsed_ms.as_deref(), expected);
        assert_eq!(reader.query_count(), 1);
    }
}

#[tokio::test]
async fn malformed_or_missing_native_elapsed_is_refused_and_the_original_source_owner_is_closed() {
    for value in
        [Some(Val::Int(i64::from(i32::MIN) - 1)), Some(Val::Int(i64::from(i32::MAX) + 1)), Some(Val::Float(1.5)), None]
    {
        let row = census_row(55, None, "Sleeping");
        let row = if let Some(value) = value {
            replace(&row, &[("session_elapsed_ms", value)])
        } else {
            Rec::of(
                &row.columns()
                    .filter(|(name, _)| *name != "session_elapsed_ms")
                    .map(|(name, value)| (name, value.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let mut reader = reader(vec![Ok(vec![row])], IdentityDetail::Endpoint);
        let result = crate::live::collect_from(&mut reader).await;
        assert_eq!(reader.io.closed, 1);
        assert!(result.is_err());
        assert_eq!(reader.query_count(), 1);
    }
}

fn idle_connection_row(visible: Val, count: Val, id: Val, connected: Val) -> Rec {
    let row = census_row(55, None, "Sleeping");
    let mut columns = row.columns().map(|(n, v)| (n, v.clone())).collect::<Vec<_>>();
    columns.extend([
        ("idle_connection_scope", Val::Bool(true)),
        ("idle_connection_visibility", visible),
        ("idle_connection_candidates", count),
        ("idle_connection_id", id),
        ("idle_connected_without_offset", connected),
    ]);
    Rec::of(&columns)
}
fn connection(row: &ActivityObservation) -> Option<&MssqlIdleSessionConnection> {
    let SessionNativeContext::Mssql(native) = row.session_context.as_deref()?.native.as_deref()? else {
        return None;
    };
    native.idle_connection.as_ref()
}
#[tokio::test]
async fn explicit_census_adds_idle_primary_connection_without_changing_live_or_request_selection() {
    let native = idle_connection_row(
        Val::Bool(true),
        Val::Int(1),
        Val::Text("a8a44410-5033-4db1-9e90-79354ab31702".into()),
        Val::Text("2019-01-01T00:00:00".into()),
    );
    let mut source = baseline_reader(vec![native]);
    let (_, rows) = crate::telemetry::activity::capture(&mut source).await.unwrap();
    assert!(matches!(connection(&rows[0]), Some(MssqlIdleSessionConnection::Observed { .. })));
    assert!(rows[0].idle && rows[0].key.request.is_none() && rows[0].statement_identity.is_none());
    assert!(rows[0].conditions(&ActivityPolicy::default()).is_empty());
    assert_eq!(source.query_count(), 3);
    let query = &source.io.calls[1].sql;
    assert!(query.contains("IF @idle_connection_scope=1 AND @idle_connection_visibility=1"));
    assert!(query.contains("SELECT TOP (2) CONVERT(varchar(36),c.connection_id)"));
    assert!(
        query.contains("c.parent_connection_id IS NULL") && query.contains("c.most_recent_session_id=s.session_id")
    );
    assert!(query.contains("current_request.session_id=s.session_id"));
    assert!(
        !query.contains("current_request.database_id"),
        "foreign-database current requests must withhold idle identity"
    );
    assert!(!query.contains("COALESCE(r.connection_id"));
    let ordinary = sql::metadata(IdentityDetail::Endpoint, page_server_reads::Availability::Unknown);
    assert!(!ordinary.contains("idle_connection") && !ordinary.contains("sys.dm_exec_connections"));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[test]
fn idle_primary_candidates_withhold_ambiguity_missing_and_restricted_without_fabricating_requests() {
    let uuid = || Val::Text("a8a44410-5033-4db1-9e90-79354ab31702".into());
    let clock = || Val::Text("2019-01-01T00:00:00".into());
    for (visible, count, id, at, expected) in [
        (Val::Bool(true), Val::Int(0), Val::Null, Val::Null, MssqlIdleSessionConnection::Missing),
        (Val::Bool(true), Val::Int(2), uuid(), clock(), MssqlIdleSessionConnection::Ambiguous),
        (
            Val::Bool(false),
            Val::Null,
            Val::Null,
            Val::Null,
            MssqlIdleSessionConnection::VisibilityUnavailable { permitted: Some(false) },
        ),
        (
            Val::Null,
            Val::Null,
            Val::Null,
            Val::Null,
            MssqlIdleSessionConnection::VisibilityUnavailable { permitted: None },
        ),
    ] {
        let row = rows::record(&idle_connection_row(visible, count, id, at)).unwrap().unwrap();
        assert_eq!(connection(&row.facts), Some(&expected));
        assert!(row.current_locator.is_none() && row.facts.request_counters.is_none());
    }
    for (count, id, at) in [
        (Val::Int(3), uuid(), clock()),
        (Val::Int(1), Val::Null, clock()),
        (Val::Int(1), uuid(), Val::Null),
        (Val::Int(0), uuid(), clock()),
        (Val::Int(1), Val::Text("not-a-native-guid".into()), clock()),
    ] {
        assert!(rows::record(&idle_connection_row(Val::Bool(true), count, id, at)).is_err());
    }
}
#[test]
fn selected_database_request_absence_does_not_authorize_idle_connection_with_a_foreign_current_request() {
    let native = idle_connection_row(
        Val::Bool(true),
        Val::Int(1),
        Val::Text("a8a44410-5033-4db1-9e90-79354ab31702".into()),
        Val::Text("2019-01-01T00:00:00".into()),
    );
    let foreign = replace(
        &native,
        &[
            ("no_current_request", Val::Int(0)),
            ("state", Val::Text("running".into())),
            ("native_session_state", Val::Text("Running".into())),
        ],
    );
    let row = rows::record(&foreign).unwrap().unwrap();
    assert!(row.facts.key.request.is_none() && !row.facts.idle);
    assert!(connection(&row.facts).is_none());
    assert!(row.current_locator.is_none());
    let mut active = census_row(55, Some(1), "Running");
    let mut columns = active.columns().map(|(n, v)| (n, v.clone())).collect::<Vec<_>>();
    columns.extend(
        native
            .columns()
            .filter(|(name, _)| name.starts_with("idle_connection") || *name == "idle_connected_without_offset")
            .map(|(n, v)| (n, v.clone())),
    );
    active = Rec::of(&columns);
    let row = rows::record(&active).unwrap().unwrap();
    assert!(connection(&row.facts).is_none());
    let query = sql::census_metadata(IdentityDetail::AzureDatabase, page_server_reads::Availability::Unknown);
    assert!(query.contains("IS_SRVROLEMEMBER('##MS_ServerStateReader##')"));
    assert!(query.contains("IN(2,3,4,8)"));
    assert!(
        !super::super::visibility::connections().split("THEN").nth(1).unwrap().contains("HAS_PERMS_BY_NAME(NULL,NULL")
    );
}

#[test]
fn azure_and_unknown_request_population_scope_never_become_idle_connection_permission() {
    for scope in [Val::Bool(false), Val::Null] {
        let row = idle_connection_row(Val::Null, Val::Null, Val::Null, Val::Null);
        let row = replace(&row, &[("idle_connection_scope", scope)]);
        let record = rows::record(&row).unwrap().unwrap();
        assert_eq!(connection(&record.facts), Some(&MssqlIdleSessionConnection::ScopeUnavailable));
        let changed = replace(&row, &[("idle_connection_visibility", Val::Bool(true))]);
        assert!(rows::record(&changed).is_err());
    }
}
