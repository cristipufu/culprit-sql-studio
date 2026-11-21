use super::*;
use culprit_core::{Engine, MeasurementScope, Snapshot};
fn evidence() -> (CollectionDescription, Vec<SettingObservation>) {
    let source = CollectionDescription {
        target: CollectionTarget::Shared(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("recorded-server").qualified("kind", "azure-sql-logical-server"),
            native: NativeIdentity::new("recorded-pool"),
        }),
        partition: SourcePartition::Target,
        source: MSSQL_POOL_SETTINGS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlPoolSettings {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Settings,
                observed_from: "2020-01-01T00:00:01Z".into(),
                observed_until: "2020-01-01T00:00:03Z".into(),
                records: 6,
                sources: vec![StateSourceCoverage::collected(MSSQL_POOL_SETTINGS_SOURCE.into(), 6)],
            },
            membership: Box::new(MssqlPoolSettingsMembership {
                database: {
                    let mut database = member_database("recorded-server");
                    database.incarnation = Some("d0e01675-5323-4923-9c60-13eb0f9f7abc".into());
                    database
                },
                elastic_pool: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Pool,
                    namespace: NativeIdentity::new("recorded-server").qualified("kind", "azure-sql-logical-server"),
                    native: NativeIdentity::new("recorded-pool"),
                }),
                elastic_pool_error: None,
                member: NativeIdentity::new("recorded-server")
                    .qualified("logical_database_guid", "d0e01675-5323-4923-9c60-13eb0f9f7abc")
                    .qualified("physical_database_guid", "c03eb217-6b8c-4373-ae62-08d21e4afc97"),
                member_observed_at: "2020-01-01T00:00:00Z".into(),
                database_writable: true,
                point: Some(MssqlPoolSettingsPoint { database_id: 7, observed_at: "2020-01-01T00:00:02Z".into() }),
            }),
        },
    };
    let rows = MSSQL_POOL_SETTING_NAMES
        .into_iter()
        .enumerate()
        .map(|(index, name)| SettingObservation {
            source: MSSQL_POOL_SETTINGS_SOURCE.into(),
            namespace: MSSQL_POOL_SETTINGS_NAMESPACE.into(),
            name: name.into(),
            scope: MeasurementScope::Pool,
            nature: SettingNature::Configuration,
            value: [Some("0"), Some("80"), Some("100"), None, Some("75"), Some("2048")][index].map(str::to_owned),
            value_in_use: None,
            value_for_secondary: None,
            unit: mssql_pool_setting_unit(name).map(str::to_owned),
            native_context: None,
            native_source: Some(MSSQL_POOL_SETTINGS_NAMESPACE.into()),
            pending_restart: None,
            native_is_default: None,
            default_value: None,
            default_source: SettingDefaultSource::Unknown,
        })
        .collect();
    (source, rows)
}
fn report(source: &CollectionDescription, rows: &[SettingObservation], complete: bool) -> Result<PoolSettingsReport> {
    explain(PoolSettingsEvidence { source, observations: &rows.iter().collect::<Vec<_>>(), read_complete: complete })
}
fn captured() -> CapturedPoolSettingsRead {
    let (source, rows) = evidence();
    let identity = CaptureIdentity {
        observation_id: "original-pool-settings".into(),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: "a".repeat(64),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let positions = rows
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            CapturedSnapshotPosition::from_export(
                &ExportedRecord::new(&source, &identity, TelemetryRecord::Setting(row.clone())).unwrap(),
                ordinal as u32,
            )
            .unwrap()
        })
        .collect();
    CapturedPoolSettingsRead::captured(source, capture, positions, rows, true).unwrap()
}
#[test]
fn current_configuration_keeps_native_units_zero_unknown_and_pool_scope_without_a_fault_verdict() {
    let (source, rows) = evidence();
    let report = report(&source, &rows, true).unwrap();
    assert_eq!(report.elastic_pool.as_ref().unwrap().kind, SharedResourceKind::Pool);
    assert_eq!(report.native_rows, 6);
    assert_eq!(report.fields.len(), 6);
    assert_eq!(report.fields[0].native_value.as_deref(), Some("0"));
    assert_eq!(report.fields[0].value, Some(format::percent_value(0.0)));
    assert_eq!(report.fields[3].native_value, None);
    assert_eq!(report.fields[3].value, None);
    assert_eq!(report.fields[5].unit, "io-operations-per-second");
    assert_eq!(report.observed_at.as_deref(), Some("2020-01-01T00:00:02Z"));
    let value = serde_json::to_value(report).unwrap();
    assert!(value.get("severity").is_none());
    assert!(!value.to_string().contains("d0e01675-5323-4923-9c60-13eb0f9f7abc"));
}
#[test]
fn archive_gaps_preserve_six_native_rows_and_membership_without_inventing_the_missing_configuration() {
    let (source, mut rows) = evidence();
    rows.pop();
    assert!(report(&source, &rows, true).is_err());
    let report = report(&source, &rows, false).unwrap();
    assert_eq!(report.native_rows, 6);
    assert_eq!(report.observed_rows, 5);
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert!(!report.fields.iter().any(|field| field.key == "pool_max_io"));
    assert!(!report.read_complete);
}
#[test]
fn foreign_engine_missing_header_database_scopes_and_unknown_column_substitutes_are_refused() {
    for mutation in ["engine", "header", "scope", "name", "unit"] {
        let (mut source, mut rows) = evidence();
        match mutation {
            "engine" => {
                let CollectionTarget::Shared(target) = &mut source.target else { panic!() };
                target.engine = Engine::Postgres;
            }
            "header" => {
                source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
            }
            "scope" => rows[0].scope = MeasurementScope::Database,
            "name" => rows[0].name = "cpu_limit".into(),
            _ => rows[0].unit = Some("vcores".into()),
        }
        assert!(report(&source, &rows, true).is_err(), "{mutation}");
    }
}
#[test]
fn original_receipts_reject_tampering_and_sharing_withdraws_membership_without_a_current_catalog_repair() {
    let original = captured();
    assert!(original.is_valid());
    let refs = original.captured_references().unwrap();
    assert_eq!(refs.len(), 6);
    assert!(refs.iter().all(|reference| reference.stream == TelemetryStream::SettingObservations));
    let mut changed = original.clone();
    changed.observations[0].value = Some("1".into());
    assert!(!changed.is_valid());
    assert!(explain_captured(&changed).is_err());
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.operations.pool_settings = Some(Box::new(RecordedPoolSettingsRead::Captured { read: Box::new(original) }));
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(matches!(shared.operations.pool_settings.as_deref(), Some(RecordedPoolSettingsRead::Unavailable { .. })));
    let rendered = crate::analyze(shared).pool_settings.unwrap();
    assert_eq!(rendered.source_state, SourceState::Unavailable);
    assert!(rendered.target.is_none() && rendered.fields.is_empty() && rendered.context.is_none());
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(serialized.contains("d0e01675-5323-4923-9c60-13eb0f9f7abc"));
}

fn member_database(server: &str) -> culprit_core::DatabaseIdentity {
    culprit_core::DatabaseIdentity::server(Engine::Mssql, "recorded-member", server, 1433, None)
}

#[test]
fn a_single_database_retains_six_native_pool_meanings_without_elastic_entitlement_or_scope_substitution() {
    for pooled in [false, true] {
        let (mut source, rows) = evidence();
        let SourceObservation::MssqlPoolSettings { membership, .. } = &mut source.observation else { panic!() };
        if !pooled {
            membership.elastic_pool = None;
        }
        source.target = membership.database.clone().into();
        let report = report(&source, &rows, true).unwrap();
        assert_eq!(report.target.as_ref(), Some(&source.target));
        assert_eq!(report.member_database.as_deref(), Some("recorded-member"));
        assert_eq!(report.logical_server.as_deref(), Some("recorded-server"));
        assert_eq!(report.elastic_pool.is_some(), pooled);
        assert_eq!(report.fields.len(), 6);
        assert_eq!(report.fields[0].native_value.as_deref(), Some("0"));
        assert!(report.fields[3].native_value.is_none());
        assert!(report.limitations.iter().any(|value| value == &t!("view.pool_settings.limit.scope")));
        assert!(rows.iter().all(|row| row.scope == MeasurementScope::Pool));
    }
}

#[test]
fn missing_elastic_metadata_preserves_settings_and_words_unknown_membership_instead_of_known_absence() {
    let (mut source, rows) = evidence();
    let SourceObservation::MssqlPoolSettings { membership, .. } = &mut source.observation else { panic!() };
    membership.elastic_pool = None;
    membership.elastic_pool_error = Some(culprit_core::ErrorCode::PermissionDenied);
    source.target = membership.database.clone().into();
    let report = report(&source, &rows, true).unwrap();
    assert_eq!(report.source_state, SourceState::Collected);
    assert_eq!(report.fields.len(), 6);
    assert_eq!(report.elastic_pool_error, Some(culprit_core::ErrorCode::PermissionDenied));
    assert_eq!(report.summary, t!("view.pool_settings.summary.membership_unavailable"));
    assert_ne!(report.summary, t!("view.pool_settings.summary.unpooled"));
}
