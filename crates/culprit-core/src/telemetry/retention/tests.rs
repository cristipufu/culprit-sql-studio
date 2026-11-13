use super::*;

fn slot(name: &str, data: Option<u64>, catalog: Option<u64>) -> RetentionObservation {
    RetentionObservation {
        source: "pg_replication_slots".into(),
        identity: NativeIdentity::new(name),
        observed_at: "2026-11-13T08:00:00.123456Z".into(),
        scope: MeasurementScope::Server,
        holder: CleanupHolder::ReplicationSlot,
        name: name.into(),
        horizons: [(CleanupHorizon::Data, "xmin", data), (CleanupHorizon::Catalog, "catalog_xmin", catalog)]
            .into_iter()
            .filter_map(|(kind, native_name, age)| {
                Some(RetentionHorizon {
                    protects: if kind == CleanupHorizon::Data {
                        vec![CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
                    } else {
                        vec![CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
                    },
                    native_name: native_name.into(),
                    native_value: "1234".into(),
                    transactions_behind: Some(age?.to_string()),
                })
            })
            .collect(),
        session: None,
        unresolved_session_id: None,
        transaction: None,
        query: None,
        client: None,
        activity: TransactionActivity::SlotInactive,
        native_state: None,
        age_seconds: None,
        database: None,
        code_ownership: PayloadUnavailable::NotCollected,
    }
}
fn snapshot(holders: Vec<RetentionObservation>) -> RetentionSnapshot {
    RetentionSnapshot {
        coverage: RetentionCoverage {
            native_sources: vec!["pg_replication_slots".into()],
            qualified_horizons: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
        },
        holders,
        sources: vec![RetentionSourceCoverage { source: "pg_replication_slots".into(), state: SourceState::Collected }],
    }
}

#[test]
fn mixed_slot_horizons_are_selected_independently_and_ties_survive() {
    let snapshot =
        snapshot(vec![slot("mixed", Some(2_000_000), Some(150_000_000)), slot("data", Some(2_000_000), None)]);
    assert!(snapshot.is_horizon_complete(CleanupHorizon::Data));
    assert_eq!(snapshot.oldest_observed(CleanupHorizon::Data).len(), 2);
    assert_eq!(snapshot.oldest_observed(CleanupHorizon::Catalog)[0].name, "mixed");
    let display = snapshot.oldest_observed(CleanupHorizon::Data)[0].display(CleanupHorizon::Data);
    assert!(!display.catalog_only);
    assert_eq!(display.xid_age, Some(2_000_000.0));
}

#[test]
fn partial_sources_retain_observed_holders_without_claiming_complete_visibility() {
    let mut snapshot = snapshot(vec![slot("observed", Some(10), None)]);
    snapshot
        .sources
        .push(RetentionSourceCoverage { source: "pg_stat_activity".into(), state: SourceState::Unavailable });
    snapshot.coverage.native_sources.push("pg_stat_activity".into());
    assert!(snapshot.is_valid());
    assert!(!snapshot.is_horizon_complete(CleanupHorizon::Data));
    assert_eq!(snapshot.oldest_observed(CleanupHorizon::Data).len(), 1);
    snapshot.sources[0].state = SourceState::Partial;
    assert!(!snapshot.is_horizon_complete(CleanupHorizon::Data));
}

#[test]
fn duplicate_native_identity_or_unknown_source_is_invalid() {
    let row = slot("slot", Some(1), None);
    assert!(!snapshot(vec![row.clone(), row.clone()]).is_valid());
    let mut unknown = snapshot(vec![row]);
    unknown.holders[0].source = "unregistered".into();
    assert!(!unknown.is_valid());
}

#[test]
fn query_context_requires_an_exact_backend_generation_and_never_establishes_code_ownership() {
    let mut row = slot("slot", Some(1), None);
    row.query = Some(RetentionQueryContext {
        identity: Some(NativeQueryIdentity { query_id: "42".into(), qualifiers: Default::default() }),
        started_at: Some("2026-11-13T07:59:00.000001Z".into()),
        relationship: RetentionQueryRelationship::PreviousStatement,
    });
    assert!(!row.is_valid());
    row.session = Some(NativeActivityIdentity { id: "100".into(), generation: "2026-11-13T07:00:00.000001Z".into() });
    assert!(row.is_valid());
    assert_eq!(row.code_ownership, PayloadUnavailable::NotCollected);
    row.session.as_mut().unwrap().generation.clear();
    assert!(!row.is_valid());
}

#[test]
fn native_backend_generations_remain_opaque_while_observation_times_require_utc_instants() {
    let mut row = slot("consumer", Some(1), None);
    row.session = Some(NativeActivityIdentity { id: "42".into(), generation: "native-generation-without-utc".into() });
    assert!(row.is_valid(), "the generic identity contract does not invent a UTC offset for native generations");
    row.observed_at = "2026-11-13T08:00:00".into();
    assert!(!row.is_valid());
}

#[test]
fn empty_sources_preserve_database_qualification_without_claiming_shared_catalog_or_recovery_completeness() {
    let mut empty = snapshot(vec![]);
    empty.sources[0].state = SourceState::Empty;
    assert!(empty.is_horizon_complete(CleanupHorizon::Data));
    assert!(empty.is_horizon_complete(CleanupHorizon::Catalog));
    assert!(!empty.is_horizon_complete(CleanupHorizon::SharedCatalog));
    let metadata = OperationalObservation {
        source: RetentionCoverage::SOURCE.into(),
        key: empty.coverage.key().unwrap(),
        fact: OperationalFact::RetentionCoverage(empty.coverage.clone()),
    };
    assert!(metadata.is_valid());
    let restored: OperationalObservation = serde_json::from_str(&serde_json::to_string(&metadata).unwrap()).unwrap();
    let OperationalFact::RetentionCoverage(coverage) = restored.fact else { panic!() };
    assert_eq!(coverage, empty.coverage);
    empty.coverage.qualified_horizons.clear();
    assert!(!empty.is_horizon_complete(CleanupHorizon::Data));
    assert!(!empty.is_horizon_complete(CleanupHorizon::Catalog));
    empty.holders.push(slot("unqualified", Some(1), None));
    empty.sources[0].state = SourceState::Collected;
    empty.holders[0].scope = MeasurementScope::Unknown;
    assert!(!empty.is_valid());
    assert!(empty.oldest_observed(CleanupHorizon::Data).is_empty());
}

#[test]
fn collection_keeps_native_client_context_while_shared_snapshots_remove_it_and_wrappers_cannot_relabel_sources() {
    let mut row = slot("consumer", Some(1), None);
    row.client = Some(ActivityClient {
        application: Some("application-worker".into()),
        user: Some("reader".into()),
        host: Some("127.0.0.1".into()),
    });
    let mut fact = OperationalFact::Retention(Box::new(row.clone()));
    fact.redact(crate::Engine::Postgres);
    let OperationalFact::Retention(retained) = &fact else { panic!() };
    assert_eq!(retained.client, row.client, "native collection follows the shared activity context policy");
    let relabeled = OperationalObservation { source: "different-source".into(), key: row.key().unwrap(), fact };
    assert!(!relabeled.is_valid());
    let mut captured: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    captured.pressure = Some(crate::Pressure { retention: Some(snapshot(vec![row])), ..Default::default() });
    let shared = crate::redact::redact_snapshot(&captured);
    assert!(shared.pressure.unwrap().retention.unwrap().holders[0].client.is_none());
}
