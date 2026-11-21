use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabasePropertiesCase {
    Exact,
    AzureCatalogNamespace,
    CatalogCreatedUnknown,
    NotInAvailabilityGroup,
    LocalPrimary,
    LocalSecondary,
    NativeLocalRoleNull,
    LocalRolePermissionDenied,
    LocalRoleUnsupported,
    Encrypted,
    LedgerEnabled,
    ChangeFeedEnabled,
    Published,
    Subscribed,
    MergePublished,
    Distributor,
    AcceleratedRecoveryEnabled,
    PartiallyContained,
    AutoShrinkEnabled,
    BrokerEnabled,
    Standby,
    DelayedDurabilityAllowed,
    DelayedDurabilityForced,
    ReadOnlyConnection,
    UnknownValues,
    NoRecordedCheckDb,
    UnknownCodes,
    NativePartial,
    Denied,
    NotApplicable,
}
impl DatabasePropertiesCase {
    pub const ALL: [Self; 30] = [
        Self::Exact,
        Self::AzureCatalogNamespace,
        Self::CatalogCreatedUnknown,
        Self::NotInAvailabilityGroup,
        Self::LocalPrimary,
        Self::LocalSecondary,
        Self::NativeLocalRoleNull,
        Self::LocalRolePermissionDenied,
        Self::LocalRoleUnsupported,
        Self::Encrypted,
        Self::LedgerEnabled,
        Self::ChangeFeedEnabled,
        Self::Published,
        Self::Subscribed,
        Self::MergePublished,
        Self::Distributor,
        Self::AcceleratedRecoveryEnabled,
        Self::PartiallyContained,
        Self::AutoShrinkEnabled,
        Self::BrokerEnabled,
        Self::Standby,
        Self::DelayedDurabilityAllowed,
        Self::DelayedDurabilityForced,
        Self::ReadOnlyConnection,
        Self::UnknownValues,
        Self::NoRecordedCheckDb,
        Self::UnknownCodes,
        Self::NativePartial,
        Self::Denied,
        Self::NotApplicable,
    ];
}

/// Controlled native-shaped facts share one genuine collection/seal/readback harness; no live support is claimed.
pub fn database_properties_evidence(case: DatabasePropertiesCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut point: DatabasePropertiesObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/database-properties.native.json"
    )))
    .unwrap();
    match case {
        DatabasePropertiesCase::AzureCatalogNamespace => {
            point.catalog_database_id = "42".into();
            point.local_replica_role = Some(MssqlDatabaseReplicaRole::Unavailable {
                observed_at: point.observed_at.clone(),
                reason: MssqlDatabaseReplicaRoleUnavailable::UnqualifiedObserver,
                native_error_number: None,
            });
        }
        DatabasePropertiesCase::CatalogCreatedUnknown => point.catalog_created_without_offset = None,
        DatabasePropertiesCase::NotInAvailabilityGroup => {
            point.local_replica_role =
                Some(MssqlDatabaseReplicaRole::NotInAvailabilityGroup { observed_at: point.observed_at.clone() });
        }
        DatabasePropertiesCase::LocalPrimary
        | DatabasePropertiesCase::LocalSecondary
        | DatabasePropertiesCase::NativeLocalRoleNull => {
            point.local_replica_role = Some(MssqlDatabaseReplicaRole::LocalAvailabilityGroup {
                source: MssqlDatabaseReplicaRoleSource::HadrDatabaseReplicaStates,
                observed_at: point.observed_at.clone(),
                replica_id: "91487244-730d-4b83-8e49-d482241c7288".into(),
                group_database_id: "71bd65c3-bd16-4d08-b5ae-d72e3bf2f6d4".into(),
                is_primary: (case != DatabasePropertiesCase::NativeLocalRoleNull)
                    .then_some(case == DatabasePropertiesCase::LocalPrimary),
            });
        }
        DatabasePropertiesCase::LocalRolePermissionDenied | DatabasePropertiesCase::LocalRoleUnsupported => {
            point.local_replica_role = Some(MssqlDatabaseReplicaRole::Unavailable {
                observed_at: point.observed_at.clone(),
                reason: if case == DatabasePropertiesCase::LocalRolePermissionDenied {
                    MssqlDatabaseReplicaRoleUnavailable::PermissionDenied
                } else {
                    MssqlDatabaseReplicaRoleUnavailable::UnsupportedLayout
                },
                native_error_number: (case == DatabasePropertiesCase::LocalRolePermissionDenied).then_some(229),
            });
        }
        DatabasePropertiesCase::Encrypted => point.encrypted = Some(true),
        DatabasePropertiesCase::LedgerEnabled => point.ledger_enabled = Some(true),
        DatabasePropertiesCase::ChangeFeedEnabled => point.change_feed_enabled = Some(true),
        DatabasePropertiesCase::Published => point.published = Some(true),
        DatabasePropertiesCase::Subscribed => point.subscribed = Some(true),
        DatabasePropertiesCase::MergePublished => point.merge_published = Some(true),
        DatabasePropertiesCase::Distributor => point.distributor = Some(true),
        DatabasePropertiesCase::AcceleratedRecoveryEnabled => point.accelerated_recovery_enabled = Some(true),
        DatabasePropertiesCase::PartiallyContained => {
            point.containment = DatabasePropertyCode { code: Some(1), description: Some("PARTIAL".into()) };
        }
        DatabasePropertiesCase::AutoShrinkEnabled => point.auto_shrink = Some(true),
        DatabasePropertiesCase::BrokerEnabled => point.broker_enabled = Some(true),
        DatabasePropertiesCase::Standby => point.in_standby = Some(true),
        DatabasePropertiesCase::DelayedDurabilityAllowed => {
            point.delayed_durability = DatabasePropertyCode { code: Some(1), description: Some("ALLOWED".into()) };
        }
        DatabasePropertiesCase::DelayedDurabilityForced => {
            point.delayed_durability = DatabasePropertyCode { code: Some(2), description: Some("FORCED".into()) };
        }
        DatabasePropertiesCase::ReadOnlyConnection => point.updateability = Some("READ_ONLY".into()),
        DatabasePropertiesCase::UnknownValues => {
            point.last_good_checkdb_without_offset = None;
            point.catalog_created_without_offset = None;
            point.local_replica_role = None;
            point.collation = None;
            point.compatibility_level = None;
            point.query_store_enabled = None;
            point.auto_shrink = None;
            point.broker_enabled = None;
            point.containment = DatabasePropertyCode { code: None, description: None };
            point.encrypted = None;
            point.ledger_enabled = None;
            point.change_feed_enabled = None;
            point.published = None;
            point.subscribed = None;
            point.merge_published = None;
            point.distributor = None;
            point.accelerated_recovery_enabled = None;
            point.in_standby = None;
            point.delayed_durability = DatabasePropertyCode { code: None, description: None };
            point.auto_create_statistics = None;
            point.auto_update_statistics = None;
            point.auto_update_statistics_async = None;
            point.target_recovery_time_seconds = None;
        }
        DatabasePropertiesCase::NoRecordedCheckDb => {
            point.last_good_checkdb_without_offset = Some("1900-01-01T00:00:00.000".into())
        }
        DatabasePropertiesCase::UnknownCodes => {
            point.catalog_state.code = Some(255);
            point.delayed_durability.code = Some(255);
            point.containment.code = Some(255);
            point.snapshot_isolation.code = None;
            point.updateability = Some("FUTURE_NATIVE_VALUE".into());
        }
        _ => {}
    }
    let denied = case == DatabasePropertiesCase::Denied;
    let not_applicable = case == DatabasePropertiesCase::NotApplicable;
    let records = if denied || not_applicable {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: DATABASE_PROPERTIES_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::DatabaseProperties(Box::new(point)),
        })]
    };
    let coverage = if denied {
        StateSourceCoverage::unavailable(
            DATABASE_PROPERTIES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else if not_applicable {
        StateSourceCoverage::not_applicable(DATABASE_PROPERTIES_SOURCE.into())
    } else {
        StateSourceCoverage::observed(
            DATABASE_PROPERTIES_SOURCE.into(),
            1,
            case != DatabasePropertiesCase::NativePartial,
        )
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            Some("native-logical-database".into()),
        )
        .into(),
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![coverage],
        }),
    };
    (source, records)
}
