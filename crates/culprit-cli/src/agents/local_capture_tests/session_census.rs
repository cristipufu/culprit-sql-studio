use super::*;
use culprit_collector::analytical::test_support::{SessionCensusCase, session_census_evidence};
use culprit_engine::LocalCaptureAnalysisKind;

#[tokio::test]
async fn actual_startup_mcp_route_reanalyzes_one_original_census_with_exact_values_and_no_native_or_private_authority()
{
    for case in SessionCensusCase::ALL {
        let (source, rows) = session_census_evidence(case);
        let has_wide_native_session_count = rows.iter().any(|row| {
            matches!(row,
            TelemetryRecord::Activity(row) if row.session_counters.logical_reads.as_deref() == Some("9007199254740993"))
        });
        let mut fixture = LocalCaptureFixture::activity(source.clone(), rows).await;
        let native = source.observation.activity().unwrap();
        let bytes = fixture.original_bytes();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = culprit_core::i18n::scope_async(
            "fr",
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::SessionCensus),
        )
        .await;
        assert!(!error, "{case:?}: {value}");
        let report = &value["session_census"];
        assert_eq!(report["title"], culprit_core::i18n::scope("en", || culprit_core::t!("view.session_census.title")));
        assert_eq!(report["native_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        assert_eq!(
            report["logical_database"],
            json!(source.target.database().map(|database| database.database.as_str()))
        );
        assert_eq!(report["source"], json!(native.coverage));
        assert_eq!(report["read_complete"], true);
        assert_eq!(report["native_rows"], native.native_rows);
        let (grouped, requests) = match case {
            SessionCensusCase::Idle
            | SessionCensusCase::Partial
            | SessionCensusCase::IdleConnection
            | SessionCensusCase::IdleConnectionScopeUnavailable
            | SessionCensusCase::IdleConnectionMissing
            | SessionCensusCase::IdleConnectionAmbiguous
            | SessionCensusCase::IdleConnectionVisibilityUnknown
            | SessionCensusCase::IdleConnectionVisibilityDenied => (1, 0),
            SessionCensusCase::Mars => (1, 2),
            SessionCensusCase::GroupedClients | SessionCensusCase::NativeSignedElapsed => (2, 3),
            SessionCensusCase::GroupUnknownTotals => (2, 2),
            SessionCensusCase::PostgresSharedLabels => (3, 2),
            SessionCensusCase::PostgresWorker => (2, 1),
            SessionCensusCase::MysqlShared | SessionCensusCase::MysqlSwitched => (2, 1),
            SessionCensusCase::MysqlWorker => (1, 0),
            _ => (0, 0),
        };
        assert_eq!(report["client_groups"]["grouped_sessions"], grouped, "{case:?}");
        assert_eq!(
            report["client_groups"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["observed_current_requests"].as_u64().unwrap())
                .sum::<u64>(),
            requests
        );
        assert_eq!(report["client_groups"]["groups_omitted"], 0);
        let expected = match case {
            SessionCensusCase::Idle
            | SessionCensusCase::Partial
            | SessionCensusCase::IdleConnection
            | SessionCensusCase::IdleConnectionScopeUnavailable
            | SessionCensusCase::IdleConnectionMissing
            | SessionCensusCase::IdleConnectionAmbiguous
            | SessionCensusCase::IdleConnectionVisibilityUnknown
            | SessionCensusCase::IdleConnectionVisibilityDenied => (Some("7"), Some("db"), None),
            SessionCensusCase::Mars
            | SessionCensusCase::GroupedClients
            | SessionCensusCase::ContradictoryMars
            | SessionCensusCase::GroupUnknownTotals
            | SessionCensusCase::NativeSignedElapsed => (Some("4"), Some("session-default"), Some("7")),
            SessionCensusCase::Dormant => (Some("0"), None, None),
            SessionCensusCase::PostgresSharedLabels | SessionCensusCase::PostgresWorker => {
                (Some("42"), Some("db"), None)
            }
            SessionCensusCase::MysqlShared | SessionCensusCase::MysqlWorker => (None, Some("db"), None),
            SessionCensusCase::MysqlSwitched => (None, Some("other"), None),
            _ => (None, None, None),
        };
        for entry in report["entries"].as_array().unwrap() {
            let fields = entry["fields"].as_array().unwrap();
            for (key, expected) in [
                ("session_database_id", expected.0),
                ("session_database_name", expected.1),
                ("request_database_id", expected.2),
            ] {
                let field = fields.iter().find(|field| field["key"] == key).unwrap();
                assert_eq!(field["value"], json!(expected), "{case:?}: {key}");
            }
        }
        if matches!(
            case,
            SessionCensusCase::IdleConnection
                | SessionCensusCase::IdleConnectionScopeUnavailable
                | SessionCensusCase::IdleConnectionMissing
                | SessionCensusCase::IdleConnectionAmbiguous
                | SessionCensusCase::IdleConnectionVisibilityUnknown
                | SessionCensusCase::IdleConnectionVisibilityDenied
        ) {
            let entry = &report["entries"][0];
            assert!(entry["request"].is_null() && entry["idle"] == true);
            let fields = entry["fields"].as_array().unwrap();
            let id = fields.iter().find(|field| field["key"] == "idle_connection_id").unwrap();
            assert_eq!(
                id["value"],
                if matches!(case, SessionCensusCase::IdleConnection) {
                    json!("37dd5a75-5bb6-4487-89a7-a6b1b0d1ca12")
                } else {
                    serde_json::Value::Null
                }
            );
            assert!(fields.iter().find(|field| field["key"] == "request_connection_id").unwrap()["value"].is_null());
        }
        assert_eq!(value["local_verification_bytes"], bytes);
        assert_eq!(value["collected_from"], fixture.manifest.started_at);
        let encoded = value.to_string();
        if has_wide_native_session_count {
            assert!(encoded.contains("9007199254740993"));
        }
        for private in [
            "manifest_sha256",
            "source_scope_id",
            "payload_sha256",
            "fixture.invalid",
            "action_target",
            "statement_identity",
        ] {
            assert!(!encoded.contains(private), "{private}");
        }
        assert!(encoded.len() <= 64 * 1024);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (error, repeated) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::SessionCensus).await;
        assert!(!error);
        assert_eq!(repeated, value);
    }
}

#[tokio::test]
async fn compact_census_keeps_original_counts_when_entry_display_is_bounded() {
    let (mut source, rows) = session_census_evidence(SessionCensusCase::Idle);
    let TelemetryRecord::Activity(template) = &rows[0] else { panic!() };
    let rows = (61..68)
        .map(|id| {
            let mut row = template.as_ref().clone();
            row.key.session.id = id.to_string();
            TelemetryRecord::from(row)
        })
        .collect::<Vec<_>>();
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.native_rows = 7;
    native.observations = 7;
    native.coverage.records = 7;
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::SessionCensus).await;
    assert!(!error, "{value}");
    let report = &value["session_census"];
    assert_eq!(report["native_rows"], 7);
    assert_eq!(report["observed_sessions"], 7);
    assert_eq!(report["entries_available"], 7);
    assert_eq!(report["entries_presented"], 5);
    assert_eq!(report["entries_omitted"], 2);
    assert_eq!(report["client_groups"]["grouped_sessions"], 7);
    assert_eq!(report["client_groups"]["groups_available"], 1);
    assert_eq!(report["client_groups"]["entries"][0]["observed_sessions"], 7);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
