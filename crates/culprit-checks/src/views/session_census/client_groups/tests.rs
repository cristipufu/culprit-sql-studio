use super::*;
use culprit_core::{SessionCensusClientGroup, SourceState};

fn read() -> ActivityCensusRead {
    let mut read = super::super::tests::recorded();
    let native = native(&mut read.observations[0]);
    native.session_database_id = Some("7".into());
    native.session_database_name = Some("original-session-db".into());
    native.elapsed_ms = Some("600000".into());
    read
}
fn native(row: &mut ActivityObservation) -> &mut MssqlSessionContext {
    let SessionNativeContext::Mssql(native) =
        row.session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap()
    else {
        panic!("MSSQL test context")
    };
    native
}
fn with_rows(mut read: ActivityCensusRead, rows: Vec<ActivityObservation>) -> ActivityCensusRead {
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.native_rows = rows.len() as u32;
    header.observations = header.native_rows;
    header.coverage.records = header.native_rows;
    read.observations = rows;
    read
}
fn request(mut row: ActivityObservation, id: &str) -> ActivityObservation {
    row.key.request = Some(NativeActivityIdentity { id: id.into(), generation: "2020-01-01T11:59:59.0000000".into() });
    row.idle = false;
    row.state = "running".into();
    row.scope = ActivityScopeBasis::RequestDatabase;
    row.request_counters = Some(ActivityCounters {
        logical_reads: Some("9999999".into()),
        writes: Some("9999999".into()),
        ..Default::default()
    });
    row.session_context.as_deref_mut().unwrap().lifecycle = SessionLifecycle::Running;
    let native = native(&mut row);
    native.native_state = "Running".into();
    native.has_current_request = true;
    native.request_database_id = Some("7".into());
    row
}
fn value<'a>(group: &'a SessionCensusClientGroup, key: &str) -> Option<&'a str> {
    group.fields.iter().find(|field| field.key == key).unwrap().value.as_deref()
}
fn report(read: &ActivityCensusRead) -> culprit_core::SessionCensusReport {
    super::super::explain(read).unwrap()
}

#[test]
fn mars_groups_sessions_once_and_keeps_measured_session_totals_separate_from_request_work_and_shared_logins() {
    let read = read();
    let mut first = request(read.observations[0].clone(), "0");
    first.session_counters.writes = Some("2".into());
    let mars = request(first.clone(), "1");
    let mut other = request(first.clone(), "0");
    other.key.session.id = "99".into();
    other.client.as_mut().unwrap().user = Some("another-login".into());
    other.session_counters.logical_reads = Some("7".into());
    other.session_counters.writes = Some("3".into());
    native(&mut other).elapsed_ms = Some("300000".into());
    let read = with_rows(read, vec![first, mars, other]);
    let analysis = report(&read);
    let groups = &analysis.client_groups;
    assert_eq!((groups.grouped_sessions, groups.ungrouped_sessions, groups.available_groups), (2, 0, 1));
    let group = &groups.entries[0];
    assert_eq!((group.observed_sessions, group.observed_requests), (2, 3));
    assert_eq!(value(group, "session_logical_reads"), Some("9007199254741000"));
    assert_eq!(value(group, "session_writes"), Some("5"));
    assert_eq!(value(group, "session_age_ms"), Some("900000"));
    assert_eq!(group.key.database.native_id(), Some("7"));
    assert_eq!(group.key.database.name(), Some("original-session-db"));
    assert!(matches!(group.key.database, culprit_core::SessionClientDatabase::CurrentSession { .. }));
    assert_eq!(analysis.entries.len(), 3);
    assert!(groups.limitations.contains(&t!("view.session_census.limit.client")));
    assert_eq!(
        serde_json::to_value(report(&serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap())).unwrap(),
        serde_json::to_value(analysis).unwrap()
    );
}

#[test]
fn every_contradictory_mars_session_projection_withdraws_the_whole_session_in_either_order() {
    for changed in
        ["application", "host", "library", "database-id", "database-name", "end", "reads", "session_writes", "elapsed"]
    {
        for reverse in [false, true] {
            let read = read();
            let first = request(read.observations[0].clone(), "0");
            let mut second = request(first.clone(), "1");
            match changed {
                "application" => second.client.as_mut().unwrap().application = Some("changed".into()),
                "host" => second.client.as_mut().unwrap().host = None,
                "library" => second.session_context.as_deref_mut().unwrap().client_library = None,
                "database-id" => native(&mut second).session_database_id = Some("8".into()),
                "database-name" => native(&mut second).session_database_name = Some("changed".into()),
                "end" => {
                    native(&mut second).times.last_request_end_without_offset =
                        Some("2020-01-01T11:59:56.0000000".into())
                }
                "reads" => second.session_counters.logical_reads = None,
                "session_writes" => second.session_counters.writes = Some("1234".into()),
                _ => native(&mut second).elapsed_ms = Some("600001".into()),
            }
            let mut rows = vec![first, second];
            if reverse {
                rows.reverse();
            }
            let report = report(&with_rows(read, rows));
            assert_eq!(
                (report.client_groups.grouped_sessions, report.client_groups.ungrouped_sessions),
                (0, 1),
                "{changed}/{reverse}"
            );
            assert!(report.client_groups.entries.is_empty());
            assert_eq!(report.entries.len(), 2);
        }
    }
}

#[test]
fn unknown_member_values_withhold_only_their_totals_and_known_zero_stays_zero() {
    let read = read();
    let mut first = read.observations[0].clone();
    first.session_counters.writes = Some("0".into());
    let mut second = first.clone();
    second.key.session.id = "99".into();
    second.session_counters.logical_reads = None;
    native(&mut second).elapsed_ms = None;
    second.session_context.as_deref_mut().unwrap().last_request_end_age_ms = None;
    native(&mut second).times.last_request_end_without_offset = None;
    let report = report(&with_rows(read, vec![first, second]));
    let group = &report.client_groups.entries[0];
    assert_eq!(group.observed_sessions, 2);
    for field in ["session_logical_reads", "session_age_ms", "latest_end"] {
        assert_eq!(value(group, field), None);
    }
    assert_eq!(value(group, "session_writes"), Some("0"));
    assert_eq!(value(group, "earliest_login"), Some("2020-01-01T11:50:00.0000000"));
}

#[test]
fn native_partial_population_keeps_observed_groups_but_a_missing_body_withholds_grouping() {
    let mut read = read();
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.native_rows = 2;
    header.coverage.complete_scope = false;
    header.coverage.state = SourceState::Partial;
    let partial = report(&read);
    assert_eq!(partial.client_groups.grouped_sessions, 1);
    assert_eq!(partial.native_rows, 2);
    assert_eq!(partial.sources[0].state, SourceState::Partial);
    assert!(partial.client_groups.limitations.contains(&t!("view.session_census.groups.limit.population")));
    let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
    header.observations = 2;
    header.coverage.records = 2;
    read.read_complete = false;
    let incomplete = report(&read);
    assert_eq!((incomplete.client_groups.grouped_sessions, incomplete.client_groups.ungrouped_sessions), (0, 1));
    assert!(incomplete.client_groups.entries.is_empty());
    assert_eq!(incomplete.entries.len(), 1);
}

#[test]
fn missing_native_database_or_unknown_local_member_never_borrows_request_or_target_context() {
    for change in ["missing-native", "missing-id", "zero-id", "unknown-member"] {
        let mut read = read();
        let row = &mut read.observations[0];
        row.database = Some("db".into());
        match change {
            "missing-native" => row.session_context.as_deref_mut().unwrap().native = None,
            "missing-id" => {
                native(row).session_database_id = None;
                native(row).session_database_name = None;
            }
            "zero-id" => {
                native(row).session_database_id = Some("0".into());
                native(row).session_database_name = None;
            }
            _ => {
                let SourceObservation::Activity(header) = &mut read.source.observation else { panic!() };
                header.epoch = None;
                header.coverage.complete_scope = false;
                header.coverage.state = SourceState::Partial;
            }
        }
        assert!(read.is_valid(), "{change}");
        let report = report(&read);
        assert_eq!(
            (report.client_groups.grouped_sessions, report.client_groups.ungrouped_sessions),
            (0, 1),
            "{change}"
        );
        assert_eq!(report.entries.len(), 1);
    }
}

#[test]
fn explicit_negative_generation_on_one_peer_withdraws_all_same_generation_groups() {
    let read = read();
    let first = request(read.observations[0].clone(), "0");
    let mut reset = first.clone();
    reset.key.request = None;
    reset.request_counters = None;
    reset.idle = true;
    let context = reset.session_context.as_deref_mut().unwrap();
    context.lifecycle = SessionLifecycle::PoolResetPrelogin;
    context.login_age_ms = None;
    let native = native(&mut reset);
    native.native_state = "Dormant".into();
    native.login_established = false;
    native.has_current_request = false;
    native.request_database_id = None;
    let report = report(&with_rows(read, vec![first, reset]));
    assert_eq!((report.client_groups.grouped_sessions, report.client_groups.ungrouped_sessions), (0, 1));
    assert_eq!(report.entries.len(), 2);
}

#[test]
fn group_presentation_cap_does_not_replace_observed_population_or_drop_raw_rows() {
    let read = read();
    let rows = (0..MAX_PRESENTED_CLIENT_GROUPS + 1)
        .map(|i| {
            let mut row = read.observations[0].clone();
            row.key.session.id = (100 + i).to_string();
            row.client.as_mut().unwrap().host = Some(format!("native-client-{i}"));
            row
        })
        .collect();
    let report = report(&with_rows(read, rows));
    let groups = &report.client_groups;
    assert_eq!((groups.grouped_sessions, groups.available_groups), (65, 65));
    assert_eq!(groups.entries.len(), MAX_PRESENTED_CLIENT_GROUPS);
    assert!(groups.entries_limited);
    assert_eq!(report.entries.len(), 65);
}

#[test]
fn integer_totals_remain_exact_above_u64_without_float_rounding_or_reusing_login_age() {
    let read = read();
    let mut first = read.observations[0].clone();
    first.session_counters.logical_reads = Some(i64::MAX.to_string());
    first.session_counters.writes = Some("0".into());
    native(&mut first).elapsed_ms = Some(i32::MAX.to_string());
    let mut second = first.clone();
    second.key.session.id = "99".into();
    let mut third = second.clone();
    third.key.session.id = "100".into();
    let report = report(&with_rows(read, vec![first, second, third]));
    let group = &report.client_groups.entries[0];
    assert_eq!(value(group, "session_logical_reads"), Some("27670116110564327421"));
    assert_eq!(value(group, "session_age_ms"), Some("6442450941"));
    assert_eq!(value(group, "session_writes"), Some("0"));
}

#[test]
fn signed_native_elapsed_keeps_consistent_mars_and_positive_peers_but_withholds_only_the_age_sum() {
    let read = read();
    let mut first = request(read.observations[0].clone(), "0");
    first.session_counters.writes = Some("2".into());
    native(&mut first).elapsed_ms = Some("-1".into());
    let mars = request(first.clone(), "1");
    let mut other = request(first.clone(), "0");
    other.key.session.id = "99".into();
    other.session_counters.logical_reads = Some("7".into());
    other.session_counters.writes = Some("3".into());
    native(&mut other).elapsed_ms = Some("300000".into());
    let original = with_rows(read, vec![first, mars, other]);
    let analysis = report(&original);
    let group = &analysis.client_groups.entries[0];
    assert_eq!((group.observed_sessions, group.observed_requests), (2, 3));
    assert_eq!(value(group, "session_age_ms"), None);
    assert_eq!(value(group, "session_logical_reads"), Some("9007199254741000"));
    assert_eq!(value(group, "session_writes"), Some("5"));
    assert_eq!(group.limitations, [t!("view.session_census.groups.limit.elapsed")]);
    assert_eq!(
        analysis
            .entries
            .iter()
            .filter(|entry| entry
                .fields
                .iter()
                .any(|field| field.key == "session_elapsed_ms" && field.value.as_deref() == Some("-1")))
            .count(),
        2
    );
    let mut changed = original;
    native(&mut changed.observations[1]).elapsed_ms = Some("1".into());
    let analysis = report(&changed);
    assert_eq!((analysis.client_groups.grouped_sessions, analysis.client_groups.ungrouped_sessions), (1, 1));
    assert_eq!(value(&analysis.client_groups.entries[0], "session_age_ms"), Some("300000"));
}

mod native_databases;
