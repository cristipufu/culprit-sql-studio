use super::*;
use crate::{Engine, MeasurementScope, ResetMarker, SourceState};
fn point(at: &str) -> MssqlPoolCurrentPoint {
    MssqlPoolCurrentPoint {
        pool: SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("logical-server").qualified("kind", "azure-sql-logical-server"),
            native: NativeIdentity::new("elastic-pool"),
        },
        member: NativeIdentity::new("logical-server")
            .qualified("logical_database_guid", "11111111-1111-1111-1111-111111111111")
            .qualified("physical_database_guid", "22222222-2222-2222-2222-222222222222"),
        database_id: 7,
        database_name: "anchor".into(),
        engine_edition: 5,
        native_tier: Some("General Purpose".into()),
        server_performance_state: Some(true),
        database_writable: true,
        observed_at: at.into(),
    }
}
fn source() -> CollectionDescription {
    let before = point("2026-11-21T00:00:00Z");
    CollectionDescription {
        target: CollectionTarget::Shared(before.pool.clone()),
        source: MSSQL_POOL_CURRENT_SOURCE.into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlPoolCurrent {
            authority: Box::new(MssqlPoolCurrentAuthority { before, after: point("2026-11-21T00:00:02Z") }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::WaitCounters,
                observed_from: "2026-11-21T00:00:01Z".into(),
                observed_until: "2026-11-21T00:00:02Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(crate::source_id::WAITS.into(), 1)],
            },
        },
    }
}
fn row() -> WaitCounterObservation {
    WaitCounterObservation {
        source: crate::source_id::WAITS.into(),
        wait_type: "WRITELOG".into(),
        mechanism: None,
        scope: MeasurementScope::Pool,
        reset: ResetMarker::Unknown,
        waiting_tasks_count: "1".into(),
        wait_time_ms: "5".into(),
        signal_wait_time_ms: "1".into(),
        max_wait_time_ms: "5".into(),
    }
}
#[test]
fn original_membership_brackets_current_data_without_claiming_process_incarnation_or_reset() {
    let source = source();
    assert!(source.partition_is_consistent() && source.observation.covers_request());
    assert!(source.matches_request(&CollectionRequest {
        subject: CollectionSubject::Shared { resource: point("2026-11-21T00:00:00Z").pool },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::WaitCounters },
        budget: Default::default(),
    }));
    let read = WaitCounterSnapshot::from_source(&source, vec![row()], true).unwrap();
    assert!(read.is_valid_for(Engine::Mssql));
    let context = read.context.as_ref().unwrap().native().unwrap();
    assert_eq!(context.database_writable, Some(true));
    assert_eq!(context.partition, SourcePartition::Target);
    assert_eq!(read.observations[0].reset, ResetMarker::Unknown);
    assert!(serde_json::from_value::<WaitCounterSnapshot>(serde_json::to_value(&read).unwrap()).unwrap().is_valid());
}
#[test]
fn native_tier_and_visibility_are_independent_of_engine_edition_and_unknown_is_never_zero() {
    for tier in ["General Purpose", "Business Critical", "Basic", "Standard", "Premium"] {
        let mut p = point("2026-11-21T00:00:00Z");
        p.native_tier = Some(tier.into());
        assert_eq!(p.scope_qualification(), MssqlPoolScopeQualification::Qualified);
        for visibility in [None, Some(false)] {
            p.server_performance_state = visibility;
            assert_eq!(p.scope_qualification(), MssqlPoolScopeQualification::VisibilityUnavailable);
        }
    }
    for tier in [None, Some("Hyperscale"), Some("unknown-future-tier")] {
        let mut p = point("2026-11-21T00:00:00Z");
        p.native_tier = tier.map(str::to_owned);
        assert!(p.is_valid());
        assert_eq!(p.scope_qualification(), MssqlPoolScopeQualification::UnqualifiedTier);
    }
}
#[test]
fn unqualified_pool_visibility_must_be_unavailable_and_cannot_publish_rows_or_healthy_empty() {
    for visibility in [None, Some(false)] {
        let mut source = source();
        let SourceObservation::MssqlPoolCurrent { authority, .. } = &mut source.observation else { panic!() };
        authority.before.server_performance_state = visibility;
        authority.after.server_performance_state = visibility;
        assert!(!source.partition_is_consistent());
        let SourceObservation::MssqlPoolCurrent { snapshot, .. } = &mut source.observation else { panic!() };
        snapshot.records = 0;
        snapshot.sources = vec![StateSourceCoverage::unavailable(
            crate::source_id::WAITS.into(),
            crate::ErrorCode::PermissionDenied,
            None,
        )];
        assert!(source.partition_is_consistent());
        let read = WaitCounterSnapshot::from_source(&source, vec![], true).unwrap();
        assert_eq!(read.sources[0].state, SourceState::Unavailable);
        assert!(WaitCounterSnapshot::from_source(&source, vec![row()], true).is_err());
    }
}
#[test]
fn changed_native_member_pool_role_database_and_clock_refuse_one_original_authority() {
    for index in 0..8 {
        let mut source = source();
        let SourceObservation::MssqlPoolCurrent { authority, .. } = &mut source.observation else { panic!() };
        match index {
            0 => authority.after.pool.native.id = "other".into(),
            1 => {
                authority
                    .after
                    .member
                    .qualifiers
                    .insert("physical_database_guid".into(), "33333333-3333-3333-3333-333333333333".into());
            }
            2 => authority.after.database_id = 8,
            3 => authority.after.database_name = "other".into(),
            4 => authority.after.database_writable = false,
            5 => authority.after.observed_at = "2026-11-20T00:00:00Z".into(),
            6 => authority.before.observed_at = "2026-11-21T00:00:02Z".into(),
            _ => authority.after.engine_edition = 3,
        }
        assert!(!source.partition_is_consistent(), "{index}");
        assert!(WaitCounterSnapshot::from_source(&source, vec![row()], true).is_err());
    }
}
#[test]
fn generic_pool_snapshot_foreign_engine_and_wrong_scope_reset_cannot_launder_pool_authority() {
    let source = source();
    for index in 0..4 {
        let mut changed = row();
        match index {
            0 => changed.scope = MeasurementScope::Database,
            1 => changed.scope = MeasurementScope::Server,
            2 => changed.reset = ResetMarker::Generation("guessed-startup".into()),
            _ => changed.source = "other".into(),
        }
        assert!(!changed.fits_source(&source));
        assert!(WaitCounterSnapshot::from_source(&source, vec![changed], true).is_err());
    }
    let mut generic = source.clone();
    generic.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
    assert!(!generic.partition_is_consistent());
    assert!(!row().fits_source(&generic));
    let mut foreign = source.clone();
    let CollectionTarget::Shared(pool) = &mut foreign.target else { panic!() };
    pool.engine = Engine::Postgres;
    assert!(!foreign.partition_is_consistent());
}
#[test]
fn nullable_native_tier_visibility_and_saved_authority_are_required_serialized_fields() {
    let source = source();
    let read = WaitCounterSnapshot::from_source(&source, vec![row()], true).unwrap();
    let original = serde_json::to_value(&read).unwrap();
    for path in ["pool_current"] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(path);
        assert!(serde_json::from_value::<WaitCounterSnapshot>(missing).is_err());
    }
    let original = serde_json::to_value(point("2026-11-21T00:00:00Z")).unwrap();
    for field in ["native_tier", "server_performance_state"] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlPoolCurrentPoint>(missing).is_err());
        let mut unknown = original.clone();
        unknown[field] = serde_json::Value::Null;
        assert!(serde_json::from_value::<MssqlPoolCurrentPoint>(unknown).unwrap().is_valid());
    }
}

#[test]
fn collector_clock_cannot_replace_pool_native_scope_authority() {
    let source = source();
    let mut read = WaitCounterSnapshot::from_source(&source, vec![row()], true).unwrap();
    read.pool_current = None;
    read.context = Some(ObservationReadContext::Collector {
        observed_from: "2026-11-21T00:00:01Z".into(),
        observed_until: "2026-11-21T00:00:02Z".into(),
    });
    assert!(!read.is_valid_for(Engine::Mssql));
}

#[test]
fn partial_original_body_preserves_native_count_without_manufacturing_empty_or_a_new_reset() {
    let source = source();
    let read = WaitCounterSnapshot::from_source(&source, vec![], false).unwrap();
    assert_eq!(read.sources[0].records, 1);
    assert!(read.observations.is_empty());
    assert!(!read.read_complete);
    assert!(read.is_valid());
    assert!(WaitCounterSnapshot::from_source(&source, vec![], true).is_err());
}
