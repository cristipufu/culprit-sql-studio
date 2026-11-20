use super::*;
fn read(active: &str, total: &str) -> SuspectPageHistoryRead {
    SuspectPageHistoryRead {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
        },
        source: StateSourceCoverage::collected(culprit_core::source_id::SUSPECT_PAGES.into(), 1),
        read_complete: true,
        observation: Some(Box::new(MssqlSuspectPageHistory {
            database: "sample".into(),
            native_database_id: "7".into(),
            observed_at: "2020-01-01T00:00:00.5000000Z".into(),
            database_writable: Some(true),
            count: active.into(),
            retained_rows: total.into(),
            last_reported_time_without_offset: None,
            reset: culprit_core::ResetMarker::Unknown,
        })),
    }
}
#[test]
fn repaired_history_and_known_zero_remain_recorded_counts_without_an_integrity_verdict() {
    for (active, total) in [("0", "0"), ("0", "3"), ("2", "4")] {
        let report = explain(&read(active, total)).unwrap();
        assert_eq!(report.active_rows.as_deref(), Some(active));
        assert_eq!(report.retained_rows.as_deref(), Some(total));
        assert_eq!(report.metrics.len(), 2);
        assert!(report.limitations.contains(&t!("view.suspect_pages.limit.zero")));
        assert!(report.limitations.contains(&t!("view.suspect_pages.limit.incarnation")));
        assert!(report.last_reported_time_without_offset.is_none());
    }
}
#[test]
fn denied_unsupported_partial_and_missing_bodies_do_not_become_empty_healthy_history() {
    for code in [culprit_core::ErrorCode::PermissionDenied, culprit_core::ErrorCode::Unsupported] {
        let mut read = read("0", "0");
        read.observation = None;
        read.source = StateSourceCoverage::unavailable(culprit_core::source_id::SUSPECT_PAGES.into(), code, None);
        let report = explain(&read).unwrap();
        assert_eq!(report.source_state, Some(SourceState::Unavailable));
        assert!(report.retained_rows.is_none());
        assert!(report.active_rows.is_none());
        assert!(report.metrics.is_empty());
        assert!(report.source_reason.is_some());
    }
    let mut read = read("2", "4");
    read.source.complete_scope = false;
    read.source.state = SourceState::Partial;
    let report = explain(&read).unwrap();
    assert_eq!(report.retained_rows.as_deref(), Some("4"));
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert!(report.limitations.contains(&t!("view.suspect_pages.limit.read")));
    read.observation = None;
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert!(report.retained_rows.is_none());
    assert_eq!(report.source_state, Some(SourceState::Partial));
}
#[test]
fn foreign_scan_or_unavailable_original_source_never_reuses_a_positive_point() {
    let mut scan: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    scan.sources.clear();
    let mut original = read("0", "3");
    original.target = culprit_core::DatabaseIdentity::server(
        culprit_core::Engine::Mssql,
        &scan.server.database,
        "fixture.invalid",
        1433,
        None,
    )
    .into();
    original.observation.as_mut().unwrap().database = scan.server.database.clone();
    assert!(original.is_valid());
    scan.operations.suspect_pages = Some(Box::new(original));
    assert!(explain_snapshot(&scan).unwrap().is_some());
    scan.record_unavailable_evidence(
        culprit_core::source_id::SUSPECT_PAGES,
        culprit_core::message!("view.suspect_pages.invalid"),
    );
    assert!(explain_snapshot(&scan).is_err());
    scan.sources.clear();
    let database = scan.server.database.clone();
    scan.server.database = "another-database".into();
    assert!(explain_snapshot(&scan).is_err());
    scan.server.database = database;
    scan.server.engine = culprit_core::Engine::Postgres;
    assert!(explain_snapshot(&scan).is_err());
}
