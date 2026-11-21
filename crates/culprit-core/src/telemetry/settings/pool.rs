//! Native resource-pool settings keep selected database ownership and optional elastic membership separate.
use super::*;

pub const MSSQL_POOL_SETTINGS_SOURCE: &str = "pool-resource-governance-settings";
pub const MSSQL_POOL_SETTINGS_NAMESPACE: &str = "sys.dm_user_db_resource_governance";
pub const MSSQL_POOL_SETTING_NAMES: [&str; 6] =
    ["min_cpu", "max_cpu", "cap_cpu", "min_memory", "max_memory", "pool_max_io"];

pub fn mssql_pool_setting_unit(name: &str) -> Option<&'static str> {
    match name {
        "min_cpu" | "max_cpu" | "cap_cpu" | "min_memory" | "max_memory" => Some("percent"),
        "pool_max_io" => Some("io-operations-per-second"),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPoolSettingsPoint {
    pub database_id: u32,
    pub observed_at: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPoolSettingsMembership {
    /// The selected native database remains independent of optional elastic-pool membership.
    pub database: crate::DatabaseIdentity,
    #[serde(deserialize_with = "Option::deserialize")]
    pub elastic_pool: Option<SharedResourceIdentity>,
    /// None means the bounded optional catalog lookup succeeded; a native NULL pool then means unpooled.
    #[serde(deserialize_with = "Option::deserialize")]
    pub elastic_pool_error: Option<crate::ErrorCode>,
    /// The original native member route: logical server plus logical and physical database GUIDs.
    pub member: NativeIdentity,
    pub member_observed_at: String,
    pub database_writable: bool,
    /// Absent when layout or permission prevents the current settings row from being read.
    #[serde(deserialize_with = "Option::deserialize")]
    pub point: Option<MssqlPoolSettingsPoint>,
}

impl MssqlPoolSettingsMembership {
    pub fn is_valid(&self) -> bool {
        self.database.comparison(&self.database).allows_comparison()
            && self.database.engine == crate::Engine::Mssql
            && self.database.incarnation.as_ref() == self.member.qualifiers.get("logical_database_guid")
            && self.elastic_pool_error.is_none_or(|error| {
                matches!(error, crate::ErrorCode::Unsupported | crate::ErrorCode::PermissionDenied)
                    && self.elastic_pool.is_none()
            })
            && self.elastic_pool.as_ref().is_none_or(|pool| {
                pool.is_valid()
                    && pool.engine == crate::Engine::Mssql
                    && pool.kind == SharedResourceKind::Pool
                    && pool.namespace.id == self.member.id
                    && pool.namespace.qualifiers.get("kind").map(String::as_str) == Some("azure-sql-logical-server")
            })
            && self.member.is_valid()
            && self.member.qualifiers.len() == 2
            && ["logical_database_guid", "physical_database_guid"].into_iter().all(|key| {
                self.member.qualifiers.get(key).is_some_and(|value| {
                    super::super::replication::native_guid(value)
                        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
                })
            })
            && self.database_writable
            && self.member_observed_at.len() <= 64
            && canonical_time(&self.member_observed_at).is_ok()
            && self.point.as_ref().is_none_or(|point| {
                point.database_id > 0
                    && point.database_id <= i32::MAX as u32
                    && point.observed_at.len() <= 64
                    && ObservationBounds::new(&self.member_observed_at, &point.observed_at).is_some()
            })
    }

    pub fn matches_snapshot(&self, snapshot: &StateSnapshotDescription) -> bool {
        if !self.is_valid()
            || !snapshot.is_valid()
            || snapshot.dataset != SnapshotDataset::Settings
            || snapshot.sources.len() != 1
            || snapshot.sources[0].source != MSSQL_POOL_SETTINGS_SOURCE
            || ObservationBounds::new(&self.member_observed_at, &snapshot.observed_from).is_none()
        {
            return false;
        }
        let source = &snapshot.sources[0];
        match &self.point {
            Some(point) => {
                snapshot.records == MSSQL_POOL_SETTING_NAMES.len() as u32
                    && source.records == snapshot.records
                    && matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                    && ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
                        .is_some_and(|bounds| bounds.contains(&point.observed_at))
            }
            None => {
                snapshot.records == 0
                    && source.records == 0
                    && source.state == crate::SourceState::Unavailable
                    && !source.complete_scope
            }
        }
    }

    pub fn matches_target(&self, target: &CollectionTarget) -> bool {
        self.is_valid()
            && target.is_valid()
            && match target {
                CollectionTarget::Database(database) => database == &self.database,
                CollectionTarget::Shared(pool) => {
                    self.elastic_pool_error.is_none() && self.elastic_pool.as_ref() == Some(pool)
                }
            }
    }
}

pub fn mssql_pool_settings_membership(source: &CollectionDescription) -> Result<Option<&MssqlPoolSettingsMembership>> {
    let SourceObservation::MssqlPoolSettings { snapshot, membership } = &source.observation else {
        return Ok(None);
    };
    if source.partition != SourcePartition::Target
        || !membership.matches_target(&source.target)
        || !membership.matches_snapshot(snapshot)
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(Some(membership))
}

impl SettingObservation {
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        if !self.is_valid() {
            return false;
        }
        let membership = match mssql_pool_settings_membership(source) {
            Ok(Some(membership)) => membership,
            Ok(None) => return self.source != MSSQL_POOL_SETTINGS_SOURCE,
            Err(_) => return false,
        };
        membership.point.is_some()
            && self.source == MSSQL_POOL_SETTINGS_SOURCE
            && self.namespace == MSSQL_POOL_SETTINGS_NAMESPACE
            && self.scope == crate::MeasurementScope::Pool
            && self.nature == SettingNature::Configuration
            && mssql_pool_setting_unit(&self.name).is_some_and(|unit| self.unit.as_deref() == Some(unit))
            && self.value.as_deref().is_none_or(|value| valid_value(&self.name, value))
            && self.value_in_use.is_none()
            && self.value_for_secondary.is_none()
            && self.native_source.as_deref() == Some(MSSQL_POOL_SETTINGS_NAMESPACE)
            && self.native_context.is_none()
            && self.pending_restart.is_none()
            && self.native_is_default.is_none()
            && self.default_value.is_none()
            && self.default_source == SettingDefaultSource::Unknown
    }
}

fn valid_value(name: &str, value: &str) -> bool {
    value.parse::<u32>().is_ok_and(|number| {
        value == number.to_string() && number <= if name == "pool_max_io" { i32::MAX as u32 } else { 100 }
    })
}

mod read;
pub use read::*;

#[cfg(test)]
mod tests;
