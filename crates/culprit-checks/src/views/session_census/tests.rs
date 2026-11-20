use super::*;

fn recorded() -> ActivityCensusRead {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/session-census.native.json"
    )))
    .unwrap()
}
fn value<'a>(entry: &'a SessionCensusEntry, key: &str) -> Option<&'a str> {
    entry.fields.iter().find(|field| field.key == key).unwrap().value.as_deref()
}
#[test]
fn idle_context_preserves_exact_totals_zero_unknowns_and_different_request_clocks_without_an_incident() {
    let read = recorded();
    let report = explain(&read).unwrap();
    assert_eq!((report.observed_sessions, report.observed_requests), (1, 0));
    assert_eq!(value(&report.entries[0], "session_logical_reads"), Some("9007199254740993"));
    assert_eq!(value(&report.entries[0], "session_cpu"), Some("0"));
    assert_eq!(value(&report.entries[0], "session_physical_reads"), None);
    assert_eq!(value(&report.entries[0], "current_elapsed_ms"), None);
    assert_ne!(value(&report.entries[0], "native_last_start"), value(&report.entries[0], "native_last_end"));
    assert!(ActivityEvidence::select(&read.observations, &ActivityPolicy::default()).unwrap().conditions.is_empty());
    let restored = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(explain(&restored).unwrap()).unwrap(), serde_json::to_value(report).unwrap());
}
#[test]
fn mars_retains_both_request_connections_but_counts_one_session_without_combining_session_counters() {
    let mut read = recorded();
    let mut first = read.observations.remove(0);
    first.idle = false;
    first.scope = ActivityScopeBasis::RequestDatabase;
    first.state = "running".into();
    first.key.request = Some(NativeActivityIdentity { id: "0".into(), generation: "current-request".into() });
    first.request_counters = Some(ActivityCounters::default());
    let context = first.session_context.as_deref_mut().unwrap();
    context.lifecycle = SessionLifecycle::Running;
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.native_state = "Running".into();
    native.has_current_request = true;
    native.request_connection_id = Some("11111111-1111-1111-1111-111111111111".into());
    let mut second = first.clone();
    second.key.request.as_mut().unwrap().id = "1".into();
    let SessionNativeContext::Mssql(native) =
        second.session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap();
    native.request_connection_id = Some("22222222-2222-2222-2222-222222222222".into());
    read.observations = vec![first, second];
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.native_rows = 2;
    header.observations = 2;
    header.coverage.records = 2;
    let report = explain(&read).unwrap();
    assert_eq!((report.observed_sessions, report.observed_requests, report.observed_rows), (1, 2, 2));
    assert_ne!(value(&report.entries[0], "request_connection_id"), value(&report.entries[1], "request_connection_id"));
    assert_eq!(value(&report.entries[0], "session_logical_reads"), value(&report.entries[1], "session_logical_reads"));
}
#[test]
fn missing_body_retains_native_counts_and_original_coverage_instead_of_a_healthy_empty_population() {
    let mut read = recorded();
    read.observations.clear();
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.native_source.state, SourceState::Collected);
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert_eq!(report.observed_sessions, 0);
    assert!(!report.read_complete);
    read.read_complete = true;
    assert!(explain(&read).is_err());
}
#[test]
fn duplicate_native_requests_foreign_database_and_mismatched_login_tokens_are_rejected() {
    for change in ["duplicate", "foreign-database", "login-token", "native-offset", "invented-connection"] {
        let mut read = recorded();
        match change {
            "duplicate" => {
                read.observations.push(read.observations[0].clone());
                let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
                header.native_rows = 2;
                header.observations = 2;
                header.coverage.records = 2;
            }
            "foreign-database" => {
                read.observations[0].scope = ActivityScopeBasis::RequestDatabase;
                read.observations[0].database = Some("foreign".into());
            }
            other => {
                let context = read.observations[0].session_context.as_deref_mut().unwrap();
                let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
                match other {
                    "login-token" => native.times.login_without_offset = "2020-01-01T11:50:01.0000000".into(),
                    "native-offset" => native.times.login_without_offset.push('Z'),
                    _ => native.request_connection_id = Some("11111111-1111-1111-1111-111111111111".into()),
                }
            }
        }
        assert!(explain(&read).is_err(), "{change}");
    }
}
#[test]
fn native_row_limit_and_presentation_limit_do_not_replace_the_original_population_with_displayed_entries() {
    let mut read = recorded();
    let original = read.observations[0].clone();
    read.observations = (0..MAX_CENSUS_PRESENTED_ROWS + 1)
        .map(|i| {
            let mut row = original.clone();
            row.key.session.id = (100 + i).to_string();
            row
        })
        .collect();
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.native_rows = read.observations.len() as u32 + 1;
    header.observations = read.observations.len() as u32;
    header.coverage =
        StateSourceCoverage::observed(culprit_core::source_id::SESSIONS.into(), header.observations, false);
    header.coverage.stopped = Some(CollectionStop::RowLimit);
    let report = explain(&read).unwrap();
    assert_eq!(report.entries.len(), MAX_CENSUS_PRESENTED_ROWS);
    assert_eq!(report.observed_rows, MAX_CENSUS_PRESENTED_ROWS as u32 + 1);
    assert_eq!(report.native_rows, report.observed_rows + 1);
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert!(report.entries_limited);
}

#[test]
fn optional_request_page_server_counts_remain_exact_and_never_turn_session_context_into_request_work() {
    let counters = ActivityCounters { page_server_reads: Some("9007199254740993".into()), ..Default::default() };
    let fields = counter_fields(&counters, CounterLifetime::CurrentRequest);
    assert_eq!(
        fields.iter().find(|field| field.key == "request_page_server_reads").unwrap().value.as_deref(),
        Some("9007199254740993")
    );
    assert!(
        !counter_fields(&counters, CounterLifetime::Session)
            .iter()
            .any(|field| field.key == "request_page_server_reads")
    );
    assert!(
        !counter_fields(&ActivityCounters::default(), CounterLifetime::CurrentRequest)
            .iter()
            .any(|field| field.key == "request_page_server_reads")
    );
}

#[test]
fn session_database_context_is_separate_from_mars_request_context_and_display_labels() {
    let mut read = recorded();
    let mut first = read.observations.remove(0);
    first.idle = false;
    first.scope = ActivityScopeBasis::RequestDatabase;
    first.state = "running".into();
    first.key.request = Some(NativeActivityIdentity { id: "0".into(), generation: "request-0".into() });
    first.request_counters = Some(ActivityCounters::default());
    let context = first.session_context.as_deref_mut().unwrap();
    context.lifecycle = SessionLifecycle::Running;
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.native_state = "Running".into();
    native.has_current_request = true;
    native.session_database_id = Some("4".into());
    native.session_database_name = Some("session-default".into());
    native.request_database_id = Some("7".into());
    let mut second = first.clone();
    second.key.request.as_mut().unwrap().id = "1".into();
    read.observations = vec![first, second];
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.native_rows = 2;
    header.observations = 2;
    header.coverage.records = 2;
    let report = explain(&read).unwrap();
    assert_eq!((report.observed_sessions, report.observed_requests), (1, 2));
    for entry in &report.entries {
        assert_eq!(value(entry, "database"), Some("db"));
        assert_eq!(value(entry, "session_database_id"), Some("4"));
        assert_eq!(value(entry, "session_database_name"), Some("session-default"));
        assert_eq!(value(entry, "request_database_id"), Some("7"));
    }
    assert!(report.limitations.contains(&t!("view.session_census.limit.database_ids")));
    assert!(read.observations.iter().all(|row| row.statement_identity.is_none()));
}

#[test]
fn missing_native_session_context_and_zero_ids_do_not_borrow_the_displayed_target_database() {
    for native_id in [None, Some("0"), Some("7")] {
        let mut read = recorded();
        let SessionNativeContext::Mssql(native) =
            read.observations[0].session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap();
        native.session_database_id = native_id.map(str::to_owned);
        native.session_database_name = None;
        native.request_database_id = None;
        let report = explain(&read).unwrap();
        let entry = &report.entries[0];
        assert_eq!(value(entry, "database"), Some("db"));
        assert_eq!(value(entry, "session_database_id"), native_id);
        assert_eq!(value(entry, "session_database_name"), None);
        assert_eq!(value(entry, "request_database_id"), None);
    }
    let mut read = recorded();
    read.observations[0].session_context = None;
    let report = explain(&read).unwrap();
    for key in ["session_database_id", "session_database_name", "request_database_id"] {
        assert_eq!(value(&report.entries[0], key), None);
    }
}
