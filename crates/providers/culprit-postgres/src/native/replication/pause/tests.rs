use super::*;

#[test]
fn denied_functions_are_absent_from_the_native_expression_tree_not_hidden_in_case_branches() {
    for version in [100_000, 130_000, 140_000, 170_000, 180_000] {
        for requested in [false, true] {
            for state in [false, true] {
                let access = ReplayPauseAccess { requested, state };
                let sql = query(version, true, "16384", access, 17).unwrap();
                assert_eq!(sql.contains("pg_catalog.pg_is_wal_replay_paused()"), requested);
                assert_eq!(sql.contains("pg_catalog.pg_get_wal_replay_pause_state()"), state && version >= 140_000);
                assert!(!sql.contains("has_function_privilege"), "admission belongs to the original metadata read");
                assert!(sql.contains("pg_catalog.pg_stat_wal_receiver"));
                assert!(sql.ends_with("LIMIT 17"));
                if !requested {
                    let field = if version >= 140_000 { "replay_pause_requested" } else { "replay_paused" };
                    assert!(sql.contains(&format!("'{field}',NULL::boolean")));
                }
                if !state || version < 140_000 {
                    assert!(sql.contains("'replay_pause_state',NULL::text"));
                }
                let request_key = if version >= 140_000 { "requested" } else { "paused" };
                assert!(sql.contains(&format!("'{request_key}',{requested}")));
                if version >= 140_000 {
                    assert!(sql.contains(&format!("'state',{state}")));
                }
            }
        }
    }
}

#[test]
fn native_access_flags_are_required_and_not_guessed_from_version_or_statistics_visibility() {
    let row = |requested: Option<&str>, state: Option<&str>| {
        Row::from_fields([
            ("replay_pause_requested_access".into(), requested.map(str::to_owned)),
            ("replay_pause_state_access".into(), state.map(str::to_owned)),
        ])
    };
    assert_eq!(
        ReplayPauseAccess::from_row(&row(Some("t"), Some("f"))).unwrap(),
        ReplayPauseAccess { requested: true, state: false }
    );
    for values in [row(None, Some("t")), row(Some("f"), None), row(Some("unknown"), Some("f"))] {
        assert!(ReplayPauseAccess::from_row(&values).is_err());
    }
    assert!(ReplayPauseAccess::from_row(&Row::from_fields([])).is_err());
}
