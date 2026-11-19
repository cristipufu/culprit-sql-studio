//! SQL Server group data-space accounting excludes version stores and transaction-log space. Historical peaks
//! and error1138 counts belong to native group statistics, never to the request selecting this group context.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlWorkloadGroupTempdbObservation {
    pub group_id: String,
    pub pool_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub statistics_started_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub current_kibibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub peak_kibibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub limit_violations: Option<String>,
    /// None is unavailable catalog evidence. Some with both native NULLs means no stored group limits.
    #[serde(deserialize_with = "Option::deserialize")]
    pub stored_limits: Option<MssqlStoredTempdbDataLimits>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlStoredTempdbDataLimits {
    #[serde(deserialize_with = "Option::deserialize")]
    pub fixed_mebibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub percent: Option<String>,
}

impl MssqlStoredTempdbDataLimits {
    pub fn is_valid(&self) -> bool {
        self.fixed_mebibytes.as_deref().is_none_or(|value| native_fraction(value, None))
            && self.percent.as_deref().is_none_or(|value| native_fraction(value, Some(100.0)))
    }
}

fn native_fraction(value: &str, maximum: Option<f64>) -> bool {
    value.len() <= 64
        && value.parse::<f64>().is_ok_and(|number| {
            number.is_finite()
                && number >= 0.0
                && maximum.is_none_or(|maximum| number <= maximum)
                && number.to_string() == value
        })
}

fn native_id(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|number| number > 0 && number.to_string() == value)
}

impl MssqlWorkloadGroupTempdbObservation {
    pub fn is_valid(&self) -> bool {
        native_id(&self.group_id)
            && native_id(&self.pool_id)
            && self.statistics_started_without_offset.as_deref().is_none_or(local_time)
            && [&self.current_kibibytes, &self.peak_kibibytes, &self.limit_violations]
                .into_iter()
                .flatten()
                .all(|value| valid_counter(value))
            && self.stored_limits.as_ref().is_none_or(MssqlStoredTempdbDataLimits::is_valid)
    }

    pub fn matches_grant(&self, grant: &ExecutionGrantObservation) -> bool {
        grant.source == crate::source_id::CURRENT_REQUEST_MEMORY
            && grant.workload_group.as_ref().is_some_and(|group| {
                group.id == self.group_id
                    && group.qualifiers.get("kind").map(String::as_str) == Some("sql-server-resource-governor-group")
            })
            && grant.resource.as_ref().is_some_and(|pool| {
                pool.id == self.pool_id
                    && pool.qualifiers.get("kind").map(String::as_str) == Some("sql-server-resource-governor")
            })
    }

    pub(super) fn matches_context(&self, context: &NativeReadContext) -> bool {
        context.has_qualified_local_owner()
            && context.target.engine() == crate::Engine::Mssql
            && context.target.database().is_some()
            && matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
                if member.kind == SharedResourceKind::Instance)
    }
}

#[cfg(test)]
mod tests;
