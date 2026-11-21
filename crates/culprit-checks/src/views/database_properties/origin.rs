//! Original native catalog/role points share the existing informational property report.
use super::*;

pub(super) fn fields(point: &DatabasePropertiesObservation) -> Vec<DatabasePropertyField> {
    let role = point.local_replica_role.as_ref();
    vec![
        field(
            "native_database_id",
            t!("view.database_properties.field.native_database_id"),
            Some(format::digits(point.native_database_id.clone())),
            None,
            None,
        ),
        field(
            "catalog_database_id",
            t!("view.database_properties.field.catalog_database_id"),
            Some(format::digits(point.catalog_database_id.clone())),
            None,
            None,
        ),
        field(
            "catalog_created_without_offset",
            t!("view.database_properties.field.catalog_created_without_offset"),
            point.catalog_created_without_offset.clone(),
            None,
            None,
        ),
        field(
            "local_replica_role",
            t!("view.database_properties.field.local_replica_role"),
            role.and_then(role_value),
            None,
            None,
        ),
        field(
            "local_replica_role_source",
            t!("view.database_properties.field.local_replica_role_source"),
            role.and_then(role_source),
            None,
            None,
        ),
        field(
            "local_replica_role_observed_at",
            t!("view.database_properties.field.local_replica_role_observed_at"),
            role.map(|role| role.observed_at().to_owned()),
            None,
            None,
        ),
        field(
            "local_replica_role_error_number",
            t!("view.database_properties.field.local_replica_role_error_number"),
            role.and_then(|role| match role {
                MssqlDatabaseReplicaRole::Unavailable { native_error_number, .. } => {
                    native_error_number.map(|number| format::digits(number.to_string()))
                }
                _ => None,
            }),
            None,
            None,
        ),
    ]
}

fn role_value(role: &MssqlDatabaseReplicaRole) -> Option<String> {
    match role {
        MssqlDatabaseReplicaRole::NotInAvailabilityGroup { .. } => {
            Some(t!("view.database_properties.role.not_in_group"))
        }
        MssqlDatabaseReplicaRole::LocalAvailabilityGroup { is_primary: Some(true), .. } => {
            Some(t!("view.database_properties.role.local_primary"))
        }
        MssqlDatabaseReplicaRole::LocalAvailabilityGroup { is_primary: Some(false), .. } => {
            Some(t!("view.database_properties.role.local_secondary"))
        }
        _ => None,
    }
}
fn role_source(role: &MssqlDatabaseReplicaRole) -> Option<String> {
    match role {
        MssqlDatabaseReplicaRole::NotInAvailabilityGroup { .. } => Some("sys.databases".into()),
        MssqlDatabaseReplicaRole::LocalAvailabilityGroup { source, .. } => Some(
            match source {
                MssqlDatabaseReplicaRoleSource::HadrDatabaseReplicaStates => "sys.dm_hadr_database_replica_states",
                MssqlDatabaseReplicaRoleSource::DatabaseReplicaStates => "sys.dm_database_replica_states",
            }
            .into(),
        ),
        MssqlDatabaseReplicaRole::Unavailable { .. } => None,
    }
}

pub(super) fn limitations(point: &DatabasePropertiesObservation) -> Vec<String> {
    let mut notes = vec![
        t!("view.database_properties.limit.catalog_origin"),
        t!("view.database_properties.limit.local_replica_role"),
    ];
    if let Some(MssqlDatabaseReplicaRole::Unavailable { reason, .. }) = &point.local_replica_role {
        use MssqlDatabaseReplicaRoleUnavailable as R;
        notes.push(match reason {
            R::UnsupportedPlatform => t!("view.database_properties.role.unavailable.platform"),
            R::UnsupportedLayout => t!("view.database_properties.role.unavailable.layout"),
            R::PermissionDenied => t!("view.database_properties.role.unavailable.permission"),
            R::VisibilityUnavailable => t!("view.database_properties.role.unavailable.visibility"),
            R::MetadataUnavailable => t!("view.database_properties.role.unavailable.metadata"),
            R::UnqualifiedObserver => t!("view.database_properties.role.unavailable.observer"),
            R::MissingLocalRow => t!("view.database_properties.role.unavailable.missing"),
            R::AmbiguousLocalRows => t!("view.database_properties.role.unavailable.ambiguous"),
            R::NativeReadFailed => t!("view.database_properties.role.unavailable.read"),
        });
    }
    notes
}
