//! Exact native cache-entry fixtures; these are not a query-hash aggregate or native cost qualification.
use culprit_core::{Engine, ResetMarker, Snapshot, SourceState, source_id, telemetry::*};

pub(crate) fn read(snapshot: &Snapshot) -> StatementGrantSnapshot {
    let statement = "SELECT value FROM dbo.grant_input";
    let row = StatementGrantStatistics {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2026-11-01T10:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: Some((statement.encode_utf16().count() * 2).to_string()),
            source_text_complete: true,
        },
        statement: DefinitionPayload::sql(statement, Engine::Mssql),
        available_columns: [
            "execution_count",
            "last_execution_time",
            "total_grant_kb",
            "total_used_grant_kb",
            "last_grant_kb",
            "last_used_grant_kb",
            "min_grant_kb",
            "min_used_grant_kb",
            "max_grant_kb",
            "max_used_grant_kb",
        ]
        .map(str::to_string)
        .to_vec(),
        executions: Some("40".into()),
        last_execution_without_offset: Some("2026-11-03T11:59:00.003".into()),
        total_grant_kibibytes: Some((2_u64 * 1024 * 1024 * 40).to_string()),
        total_used_kibibytes: Some((40_u64 * 1024 * 40).to_string()),
        last_grant_kibibytes: Some((2_u64 * 1024 * 1024).to_string()),
        last_used_kibibytes: Some((40_u64 * 1024).to_string()),
        min_grant_kibibytes: Some((1024_u64 * 1024).to_string()),
        min_used_kibibytes: Some((10_u64 * 1024).to_string()),
        max_grant_kibibytes: Some((3_u64 * 1024 * 1024).to_string()),
        max_used_kibibytes: Some((80_u64 * 1024).to_string()),
        reset: ResetMarker::Unknown,
    };
    let source = source_id::GRANTS.to_string();
    StatementGrantSnapshot {
        capture: CaptureIdentity { observation_id: "grant-read-a".into(), revision: None },
        description: CollectionDescription {
            target: CollectionTarget::Database(snapshot.database_identity()),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("test-instance-namespace"),
                    native: NativeIdentity::new("native-member-a"),
                }),
                epoch: Some("native-process-generation-1".into()),
            },
            source: source.clone(),
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::StatementGrants,
                observed_from: "2026-11-03T12:00:00Z".into(),
                observed_until: "2026-11-03T12:00:00.020Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(source, 1, false)],
            }),
            continuity: SourceContinuity::Unverified,
        },
        observations: vec![row],
    }
}

pub(crate) fn install(snapshot: &mut Snapshot) {
    let read = read(snapshot);
    assert!(read.is_valid());
    snapshot.pressure.get_or_insert_default().grants = vec![read];
    snapshot.sources.retain(|source| source.source != source_id::GRANTS);
    snapshot.sources.push(culprit_core::SourceOutcome {
        source: source_id::GRANTS.into(),
        state: SourceState::Partial,
        reason: Some("Bounded native cache selection".into()),
        reason_message: None,
    });
}
