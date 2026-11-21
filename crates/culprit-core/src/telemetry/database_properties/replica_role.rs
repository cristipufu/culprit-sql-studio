//! One native local availability-group verdict; no routing, geo-primary or action authority.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlDatabaseReplicaRoleSource {
    HadrDatabaseReplicaStates,
    DatabaseReplicaStates,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlDatabaseReplicaRoleUnavailable {
    UnsupportedPlatform,
    UnsupportedLayout,
    PermissionDenied,
    VisibilityUnavailable,
    MetadataUnavailable,
    UnqualifiedObserver,
    MissingLocalRow,
    AmbiguousLocalRows,
    NativeReadFailed,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MssqlDatabaseReplicaRole {
    /// A visible online boxed/MI catalog explicitly reported both membership IDs NULL.
    NotInAvailabilityGroup { observed_at: String },
    LocalAvailabilityGroup {
        source: MssqlDatabaseReplicaRoleSource,
        observed_at: String,
        replica_id: String,
        group_database_id: String,
        #[serde(deserialize_with = "Option::deserialize")]
        is_primary: Option<bool>,
    },
    Unavailable {
        observed_at: String,
        reason: MssqlDatabaseReplicaRoleUnavailable,
        #[serde(deserialize_with = "Option::deserialize")]
        native_error_number: Option<u32>,
    },
}
impl MssqlDatabaseReplicaRole {
    pub fn observed_at(&self) -> &str {
        match self {
            Self::NotInAvailabilityGroup { observed_at }
            | Self::LocalAvailabilityGroup { observed_at, .. }
            | Self::Unavailable { observed_at, .. } => observed_at,
        }
    }

    pub(super) fn is_valid_for(&self, point: &DatabasePropertiesObservation) -> bool {
        if ObservationBounds::new(self.observed_at(), &point.observed_at).is_none() {
            return false;
        }
        match self {
            Self::NotInAvailabilityGroup { .. } => point.native_database_id == point.catalog_database_id,
            Self::LocalAvailabilityGroup { source, replica_id, group_database_id, .. } => {
                (*source != MssqlDatabaseReplicaRoleSource::HadrDatabaseReplicaStates
                    || point.native_database_id == point.catalog_database_id)
                    && [replica_id, group_database_id].into_iter().all(|id| {
                        super::super::replication::native_guid(id)
                            && id.bytes().any(|byte| byte != b'0' && byte != b'-')
                    })
            }
            Self::Unavailable { native_error_number, .. } => {
                native_error_number.is_none_or(|value| value > 0 && i32::try_from(value).is_ok())
            }
        }
    }
}
