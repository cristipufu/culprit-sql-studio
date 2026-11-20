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
        (SessionCensusCase::PostgresSharedLabels, None, None, None),
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
                        changed.session_context.as_deref_mut().unwrap().native.as_deref_mut().unwrap();
                    native.session_database_id = Some("8".into());
                    assert!(changed.is_valid());
                    assert!(reference.validate_canonical_record(&source, &TelemetryRecord::from(changed)).is_err());
                }
            }
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}
