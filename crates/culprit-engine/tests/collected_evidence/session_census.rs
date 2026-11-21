use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, SessionCensusCase},
};
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
async fn original_idle_dormant_mars_unknown_and_source_gaps_reuse_the_same_census_without_a_native_query() {
    for case in SessionCensusCase::ALL {
        let (source, rows) = test_support::session_census_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        test_support::assert_activity_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
        let original = ActivityCensusRead::new(
            source.clone(),
            rows.into_iter()
                .map(|row| match row {
                    TelemetryRecord::Activity(row) => *row,
                    _ => panic!(),
                })
                .collect(),
            true,
        )
        .unwrap();
        let expected = culprit_checks::session_census::explain(&original).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.context, expected.context);
        assert_eq!(analysis.report.observed_at, expected.observed_at);
        assert_eq!(analysis.report.scope, expected.scope);
        assert_eq!(analysis.report.native_source, expected.native_source);
        assert_eq!(analysis.report.observed_sessions, expected.observed_sessions);
        assert_eq!(analysis.report.observed_requests, expected.observed_requests);
        assert_eq!(analysis.report.entries.len(), expected.entries.len());
        assert_eq!(analysis.report.client_groups.entries, expected.client_groups.entries);
        assert_eq!(analysis.report.client_groups.grouped_sessions, expected.client_groups.grouped_sessions);
        assert_eq!(analysis.report.client_groups.ungrouped_sessions, expected.client_groups.ungrouped_sessions);
        for (actual, expected) in analysis.report.entries.iter().zip(expected.entries) {
            assert_eq!(actual.activity, expected.activity);
            assert_eq!(actual.fields, expected.fields);
            assert!(actual.reference.as_ref().unwrap().is_valid());
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_mars_request_body_preserves_original_native_population_and_does_not_hydrate_or_sum_session_totals() {
    let (source, rows) = test_support::session_census_evidence(SessionCensusCase::Mars);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::ActivityObservations);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.native_source.records, 2);
    assert_eq!(analysis.report.native_source.state, SourceState::Collected);
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert_eq!((analysis.report.observed_sessions, analysis.report.observed_requests), (1, 1));
    assert_eq!(analysis.report.entries.len(), 1);
    assert!(!analysis.report.read_complete);
    assert_eq!(
        (analysis.report.client_groups.grouped_sessions, analysis.report.client_groups.ungrouped_sessions),
        (0, 1)
    );
    assert!(analysis.report.client_groups.entries.is_empty());
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn shared_postgres_caller_labels_keep_distinct_native_receipts_and_idle_last_sql_without_business_ownership() {
    let (source, rows) = test_support::session_census_evidence(SessionCensusCase::PostgresSharedLabels);
    let fixture = Fixture::new(source.clone(), rows).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
    let report = &analysis.report;
    assert_eq!((report.observed_sessions, report.observed_requests), (3, 2));
    assert_eq!(report.context.target.engine(), culprit_core::Engine::Postgres);
    assert!(report.limitations.contains(&culprit_core::t!("view.session_census.limit.client")));
    let mut references = std::collections::BTreeSet::new();
    for entry in &report.entries {
        let field = |name: &str| entry.fields.iter().find(|field| field.key == name).unwrap().value.as_deref();
        assert_eq!(field("application"), Some("fixture-shared-pooled-driver"));
        assert_eq!(field("login"), Some("shared-login"));
        assert_eq!(field("library"), None);
        assert_eq!(field("session_cpu"), None);
        let reference = entry.reference.as_ref().unwrap();
        assert!(references.insert(serde_json::to_string(reference).unwrap()));
        let exported = capture
            .records()
            .iter()
            .find(|row| matches!(&row.row().record,TelemetryRecord::Activity(row) if row.key==entry.activity))
            .unwrap();
        reference.validate_canonical_record(&source, &exported.row().record).unwrap();
        let TelemetryRecord::Activity(original) = &exported.row().record else { unreachable!() };
        let mut another_generation = original.as_ref().clone();
        another_generation.key.session.generation = "2020-01-01T11:58:00.000001Z".into();
        assert!(reference.validate_canonical_record(&source, &TelemetryRecord::from(another_generation)).is_err());
        if entry.idle {
            assert!(entry.activity.request.is_none());
            assert!(original.statement_identity.is_none());
            assert!(matches!(
                original.statement,
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
            ));
        }
    }
    assert_eq!(fixture.recovery_object_reads(), before);
}

#[tokio::test]
async fn original_native_session_database_facts_survive_replay_without_becoming_request_sql_or_scope_proof() {
    for (case, session_id, session_name, request_id) in [
        (SessionCensusCase::Idle, Some("7"), Some("db"), None),
        (SessionCensusCase::Dormant, Some("0"), None, None),
        (SessionCensusCase::Mars, Some("4"), Some("session-default"), Some("7")),
        (SessionCensusCase::Unknown, None, None, None),
        (SessionCensusCase::PostgresSharedLabels, Some("42"), Some("db"), None),
    ] {
        let (source, rows) = test_support::session_census_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.entries.len(), rows.len());
        assert!(!rows.is_empty());
        for (entry, native_row) in analysis.report.entries.iter().zip(&rows) {
            let value = |key: &str| entry.fields.iter().find(|field| field.key == key).unwrap().value.as_deref();
            assert_eq!(value("session_database_id"), session_id, "{case:?}");
            assert_eq!(value("session_database_name"), session_name, "{case:?}");
            assert_eq!(value("request_database_id"), request_id, "{case:?}");
            let reference = entry.reference.as_ref().unwrap();
            reference.validate_canonical_record(&source, native_row).unwrap();
            if let TelemetryRecord::Activity(original) = native_row {
                assert_eq!(entry.activity, original.key);
                assert_eq!(entry.scope, original.scope);
                if matches!(case, SessionCensusCase::Mars) {
                    assert!(original.statement_identity.is_none());
                    let mut another_source = source.clone();
                    let CollectionTarget::Database(database) = &mut another_source.target else { panic!() };
                    let culprit_core::DatabaseLocation::Server { host, .. } = &mut database.location else { panic!() };
                    *host = "another-native-owner.invalid".into();
                    assert!(
                        reference.validate_canonical_record(&another_source, native_row).is_err(),
                        "equal native DBIDs cannot cross source namespaces"
                    );
                    let mut changed = original.as_ref().clone();
                    let SessionNativeContext::Mssql(native) =
                        changed.session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap()
                    else {
                        panic!("MSSQL test context")
                    };
                    native.session_database_id = Some("8".into());
                    assert!(changed.is_valid());
                    assert!(reference.validate_canonical_record(&source, &TelemetryRecord::from(changed)).is_err());
                }
            }
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn original_observed_client_groups_keep_exact_native_session_age_and_conflicting_mars_refusal_offline() {
    for (case, sessions, requests) in [
        (SessionCensusCase::GroupedClients, 2, 3),
        (SessionCensusCase::GroupUnknownTotals, 2, 2),
        (SessionCensusCase::ContradictoryMars, 0, 0),
        (SessionCensusCase::NativeSignedElapsed, 2, 3),
    ] {
        let (source, rows) = test_support::session_census_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.client_groups.grouped_sessions, sessions, "{case:?}");
        assert_eq!(
            analysis.report.client_groups.entries.iter().map(|group| group.observed_requests).sum::<u32>(),
            requests
        );
        if let Some(group) = analysis.report.client_groups.entries.first() {
            let value = |key: &str| group.fields.iter().find(|field| field.key == key).unwrap().value.as_deref();
            if matches!(case, SessionCensusCase::GroupedClients) {
                assert_eq!(value("session_age_ms"), Some("900000"));
                assert_eq!(value("session_logical_reads"), Some("9007199254741000"));
                assert_eq!(value("session_writes"), Some("5"));
            } else if matches!(case, SessionCensusCase::NativeSignedElapsed) {
                assert_eq!(value("session_age_ms"), None);
                assert_eq!(value("session_logical_reads"), Some("9007199254741000"));
                assert_eq!(value("session_writes"), Some("5"));
                assert!(group.limitations.contains(&culprit_core::t!("view.session_census.groups.limit.elapsed")));
                assert_eq!(
                    analysis
                        .report
                        .entries
                        .iter()
                        .filter(|entry| entry
                            .fields
                            .iter()
                            .any(|field| field.key == "session_elapsed_ms" && field.value.as_deref() == Some("-1")))
                        .count(),
                    2
                );
            } else {
                assert_eq!(value("session_age_ms"), None);
                assert_eq!(value("session_logical_reads"), None);
                assert_eq!(value("session_writes"), Some("0"));
            }
        } else {
            assert_eq!(analysis.report.client_groups.ungrouped_sessions, 1);
        }
        for entry in &analysis.report.entries {
            let original = rows
                .iter()
                .find(|record| matches!(record, TelemetryRecord::Activity(row) if row.key == entry.activity))
                .unwrap();
            entry.reference.as_ref().unwrap().validate_canonical_record(&source, original).unwrap();
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn native_backend_and_mutable_default_groups_replay_original_scope_kind_and_unknown_work_without_collection() {
    for (case, sessions, requests) in [
        (SessionCensusCase::PostgresSharedLabels, 3, 2),
        (SessionCensusCase::PostgresWorker, 2, 1),
        (SessionCensusCase::MysqlShared, 2, 1),
        (SessionCensusCase::MysqlSwitched, 2, 1),
        (SessionCensusCase::MysqlWorker, 1, 0),
    ] {
        let (source, rows) = test_support::session_census_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let captured = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let analysis = culprit_engine::collected_resources::session_census(&captured).unwrap();
        let groups = &analysis.report.client_groups;
        assert_eq!(groups.grouped_sessions, sessions, "{case:?}");
        assert_eq!(groups.entries.iter().map(|group| group.observed_requests).sum::<u32>(), requests);
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.observed_rows as usize, rows.len());
        for group in &groups.entries {
            match case {
                SessionCensusCase::PostgresSharedLabels | SessionCensusCase::PostgresWorker => {
                    assert!(
                        matches!(&group.key.database, culprit_core::SessionClientDatabase::Backend { native_id, .. } if native_id=="42")
                    );
                    assert!(
                        group
                            .fields
                            .iter()
                            .find(|field| field.key == "earliest_backend_start_utc")
                            .unwrap()
                            .value
                            .as_ref()
                            .unwrap()
                            .ends_with('Z')
                    );
                }
                _ => assert_eq!(
                    group.key.database,
                    culprit_core::SessionClientDatabase::CurrentDefault {
                        name: if matches!(case, SessionCensusCase::MysqlSwitched) { "other" } else { "db" }.into()
                    }
                ),
            }
            for key in ["session_age_ms", "session_logical_reads", "session_writes"] {
                assert!(group.fields.iter().find(|field| field.key == key).unwrap().value.is_none());
            }
        }
        for (entry, original) in analysis.report.entries.iter().zip(&rows) {
            entry.reference.as_ref().unwrap().validate_canonical_record(&source, original).unwrap();
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}
