use super::*;

pub fn request_memory_evidence(
    statement: DefinitionPayload,
    associated: bool,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    let source = culprit_core::source_id::CURRENT_REQUEST_MEMORY;
    let mut request = activity_sample();
    let has_statement = statement.is_available();
    request.statement = statement;
    request.client = Some(ActivityClient { application: Some("application-worker".into()), user: None, host: None });
    let grant = ExecutionGrantObservation {
        source: source.into(),
        identity: NativeIdentity::new("61:0").qualified("requested_without_offset", "2020-01-01T10:00:00.003"),
        observed_at: "2020-01-01T12:00:00.000000000Z".into(),
        state: ExecutionGrantState::Granted,
        request_association: if associated {
            RequestGrantAssociation::GenerationChecked
        } else {
            RequestGrantAssociation::MissingRequest
        },
        statement_relationship: if associated && has_statement {
            GrantStatementRelationship::ObservedCurrentStatement
        } else {
            GrantStatementRelationship::Unresolved
        },
        request: associated.then_some(request),
        scope: associated.then_some(culprit_core::MeasurementScope::Database),
        requested_without_offset: "2020-01-01T10:00:00.003".into(),
        granted_without_offset: Some("2020-01-01T10:00:00.006".into()),
        requested_kibibytes: Some("9007199254740993".into()),
        required_kibibytes: Some("64".into()),
        granted_kibibytes: Some("1024".into()),
        used_kibibytes: Some("0".into()),
        maximum_used_kibibytes: None,
        ideal_kibibytes: None,
        wait_ms: None,
        timeout_seconds: Some("25".into()),
        degree_of_parallelism: Some("1".into()),
        reserved_workers: None,
        used_workers: None,
        maximum_used_workers: None,
        resource: None,
        workload_group: None,
        native_properties: [("resource_semaphore_id".into(), "0".into()), ("queue_id".into(), "3".into())].into(),
        available_columns: [
            "session_id",
            "request_id",
            "request_time",
            "grant_time",
            "requested_memory_kb",
            "required_memory_kb",
            "granted_memory_kb",
            "used_memory_kb",
            "max_used_memory_kb",
            "ideal_memory_kb",
            "wait_time_ms",
            "timeout_sec",
            "dop",
            "pool_id",
            "group_id",
            "resource_semaphore_id",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        native_null_columns: ["max_used_memory_kb", "ideal_memory_kb", "wait_time_ms", "pool_id", "group_id"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    assert!(grant.is_valid());
    let key = grant.key().unwrap();
    let records = vec![TelemetryRecord::Operational(OperationalObservation {
        source: source.into(),
        key,
        fact: OperationalFact::ExecutionGrant(Box::new(grant)),
    })];
    manifest.source.source = source.into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::CurrentRequestMemory,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: 1,
        sources: vec![StateSourceCoverage::observed(source.into(), 1, associated)],
    });
    (manifest.source, records)
}

pub fn retention_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source = "pg_replication_slots";
    let holder = RetentionObservation {
        source: source.into(),
        identity: NativeIdentity::new("replication-slot").qualified("slot_type", "logical"),
        observed_at: "2020-01-01T12:00:00.123456Z".into(),
        scope: culprit_core::MeasurementScope::Server,
        holder: culprit_core::CleanupHolder::ReplicationSlot,
        name: "replication-slot".into(),
        horizons: vec![
            RetentionHorizon {
                protects: vec![CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog],
                native_name: "xmin".into(),
                native_value: "1234".into(),
                transactions_behind: Some("200000000".into()),
            },
            RetentionHorizon {
                protects: vec![CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog],
                native_name: "catalog_xmin".into(),
                native_value: "4321".into(),
                transactions_behind: None,
            },
        ],
        session: None,
        unresolved_session_id: Some("99".into()),
        transaction: None,
        query: None,
        client: Some(ActivityClient { application: Some("replication-worker".into()), user: None, host: None }),
        activity: culprit_core::TransactionActivity::SlotInactive,
        native_state: Some("inactive".into()),
        age_seconds: None,
        database: None,
        code_ownership: PayloadUnavailable::NotCollected,
    };
    assert!(holder.is_valid());
    retention_capture(Some(holder), vec![CleanupHorizon::Data, CleanupHorizon::Catalog], false)
}

pub fn retention_coverage_only_evidence(
    qualified_horizons: Vec<CleanupHorizon>,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    retention_capture(None, qualified_horizons, true)
}

fn retention_capture(
    holder: Option<RetentionObservation>,
    qualified_horizons: Vec<CleanupHorizon>,
    complete_scope: bool,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    let CollectionTarget::Database(database) = &mut manifest.source.target else { panic!() };
    database.engine = culprit_core::Engine::Postgres;
    let source = "pg_replication_slots";
    let coverage = RetentionCoverage { native_sources: vec![source.into()], qualified_horizons };
    assert!(coverage.is_valid());
    let mut records = vec![TelemetryRecord::Operational(OperationalObservation {
        source: RetentionCoverage::SOURCE.into(),
        key: coverage.key().unwrap(),
        fact: OperationalFact::RetentionCoverage(coverage),
    })];
    let native_rows = u32::from(holder.is_some());
    if let Some(holder) = holder {
        records.push(TelemetryRecord::Operational(OperationalObservation {
            source: holder.source.clone(),
            key: holder.key().unwrap(),
            fact: OperationalFact::Retention(Box::new(holder)),
        }));
    }
    manifest.source.source = "cleanup-holders".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Cleanup,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: native_rows + 1,
        sources: vec![
            StateSourceCoverage::collected(RetentionCoverage::SOURCE.into(), 1),
            StateSourceCoverage::observed(source.into(), native_rows, complete_scope),
        ],
    });
    (manifest.source, records)
}
