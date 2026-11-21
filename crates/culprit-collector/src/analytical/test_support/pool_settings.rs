//! One native-shaped Settings corpus for projection/recovery/SDK/CLI; fixture facts are not native qualification.
use super::*;
use culprit_core::{Engine, ErrorCode, MeasurementScope};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolSettingsCase {
    Exact,
    SingleDatabase,
    PooledDatabase,
    MembershipDenied,
    MembershipUnavailable,
    NullValues,
    ZeroValues,
    NativePartial,
    Denied,
    MissingLayout,
}
impl PoolSettingsCase {
    pub const ALL: [Self; 10] = [
        Self::Exact,
        Self::SingleDatabase,
        Self::PooledDatabase,
        Self::MembershipDenied,
        Self::MembershipUnavailable,
        Self::NullValues,
        Self::ZeroValues,
        Self::NativePartial,
        Self::Denied,
        Self::MissingLayout,
    ];
}
pub fn pool_settings_evidence(case: PoolSettingsCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let available = !matches!(case, PoolSettingsCase::Denied | PoolSettingsCase::MissingLayout);
    let rows = if available {
        MSSQL_POOL_SETTING_NAMES
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let value = match case {
                    PoolSettingsCase::NullValues => None,
                    PoolSettingsCase::ZeroValues => Some("0".into()),
                    _ => [Some("0"), Some("80"), Some("100"), None, Some("75"), Some("2048")][index].map(str::to_owned),
                };
                TelemetryRecord::Setting(SettingObservation {
                    source: MSSQL_POOL_SETTINGS_SOURCE.into(),
                    namespace: MSSQL_POOL_SETTINGS_NAMESPACE.into(),
                    name: name.into(),
                    scope: MeasurementScope::Pool,
                    nature: SettingNature::Configuration,
                    value,
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
            })
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let coverage = if available {
        StateSourceCoverage::observed(MSSQL_POOL_SETTINGS_SOURCE.into(), 6, case != PoolSettingsCase::NativePartial)
    } else {
        StateSourceCoverage::unavailable(
            MSSQL_POOL_SETTINGS_SOURCE.into(),
            if case == PoolSettingsCase::Denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported },
            None,
        )
    };
    let mut source = CollectionDescription {
        target: CollectionTarget::Shared(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("pool-logical-server").qualified("kind", "azure-sql-logical-server"),
            native: NativeIdentity::new("recorded-elastic-pool"),
        }),
        partition: SourcePartition::Target,
        source: MSSQL_POOL_SETTINGS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlPoolSettings {
            membership: Box::new(MssqlPoolSettingsMembership {
                database: {
                    let mut database = member_database("pool-logical-server");
                    database.incarnation = Some("11111111-1111-1111-1111-111111111111".into());
                    database
                },
                elastic_pool: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Pool,
                    namespace: NativeIdentity::new("pool-logical-server").qualified("kind", "azure-sql-logical-server"),
                    native: NativeIdentity::new("recorded-elastic-pool"),
                }),
                elastic_pool_error: None,
                member: NativeIdentity::new("pool-logical-server")
                    .qualified("logical_database_guid", "11111111-1111-1111-1111-111111111111")
                    .qualified("physical_database_guid", "22222222-2222-2222-2222-222222222222"),
                member_observed_at: "2026-11-01T00:00:00Z".into(),
                database_writable: true,
                point: available
                    .then(|| MssqlPoolSettingsPoint { database_id: 7, observed_at: "2026-11-01T00:00:02Z".into() }),
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Settings,
                observed_from: "2026-11-01T00:00:01Z".into(),
                observed_until: "2026-11-01T00:00:03Z".into(),
                records: rows.len() as u32,
                sources: vec![coverage],
            },
        },
    };
    if matches!(
        case,
        PoolSettingsCase::SingleDatabase
            | PoolSettingsCase::PooledDatabase
            | PoolSettingsCase::MembershipDenied
            | PoolSettingsCase::MembershipUnavailable
    ) {
        let SourceObservation::MssqlPoolSettings { membership, .. } = &mut source.observation else { unreachable!() };
        if case != PoolSettingsCase::PooledDatabase {
            membership.elastic_pool = None;
        }
        membership.elastic_pool_error = match case {
            PoolSettingsCase::MembershipDenied => Some(ErrorCode::PermissionDenied),
            PoolSettingsCase::MembershipUnavailable => Some(ErrorCode::Unsupported),
            _ => None,
        };
        source.target = membership.database.clone().into();
    }
    assert!(source.partition_is_consistent());
    (source, rows)
}

pub fn assert_pool_settings_capture(
    capture: &super::super::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().iter().map(|row| row.row().record.clone()).collect::<Vec<_>>(), rows);
    assert!(capture.is_read_complete());
    let membership = mssql_pool_settings_membership(capture.source()).unwrap().unwrap();
    assert_eq!(membership.member_observed_at, "2026-11-01T00:00:00Z");
    assert_eq!(
        membership.point.as_ref().map(|point| point.observed_at.as_str()),
        (!rows.is_empty()).then_some("2026-11-01T00:00:02Z")
    );
    for row in capture.records() {
        let TelemetryRecord::Setting(value) = &row.row().record else { panic!() };
        assert!(value.fits_source(source));
    }
}

fn member_database(server: &str) -> culprit_core::DatabaseIdentity {
    culprit_core::DatabaseIdentity::server(Engine::Mssql, "recorded-member", server, 1433, None)
}
