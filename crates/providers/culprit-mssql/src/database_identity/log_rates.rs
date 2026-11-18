//! The same selected governance row supplies configuration facts; no additional native read is performed.
use super::*;
use culprit_core::telemetry::ConfiguredLogRateLimits;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Detail {
    #[default]
    Identity,
    ConfiguredLogRates,
}

pub(crate) const COLUMNS: &str = "database_id AS configured_log_database_id,primary_group_id AS configured_log_group_id,primary_max_log_rate AS configured_log_workload_group,pool_max_log_rate AS configured_log_resource_pool,instance_max_log_rate AS configured_log_instance";
pub(crate) const CLOCK: &str = "CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS configured_log_observed_at";
/// Only the requested system-view object and four fixed native columns are inspected. Hidden or changed
/// metadata cannot authorize a SQL projection which would erase otherwise readable query history.
pub(crate) const CAPABILITY: &str = "CASE WHEN (SELECT COUNT_BIG(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_user_db_resource_governance') AND ((name IN(N'primary_max_log_rate',N'pool_max_log_rate',N'instance_max_log_rate') AND system_type_id=TYPE_ID(N'bigint')) OR (name=N'primary_group_id' AND system_type_id=TYPE_ID(N'int'))))=4 THEN 1 ELSE 0 END";

pub(crate) fn capability(row: &Rec) -> Result<bool> {
    match integer(row, "configured_log_rate_capability")? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_data()),
    }
}

pub(crate) fn decode(row: &Rec, context: &Context, clock: &str) -> Result<Option<ConfiguredLogRateLimits>> {
    let Some(incarnation) = &context.incarnation else { return Ok(None) };
    let point = ConfiguredLogRateLimits {
        observed_at: optional_text(row, clock)?.map(|at| culprit_core::telemetry::canonical_time(&at)).transpose()?,
        database_id: integer(row, "configured_log_database_id")?.to_string(),
        database_incarnation: incarnation.clone(),
        workload_group_id: crate::read::optional_counter(row, "configured_log_group_id")?,
        workload_group_bytes_per_second: crate::read::optional_counter(row, "configured_log_workload_group")?,
        resource_pool_bytes_per_second: crate::read::optional_counter(row, "configured_log_resource_pool")?,
        instance_bytes_per_second: crate::read::optional_counter(row, "configured_log_instance")?,
    };
    point.is_valid().then_some(Some(point)).ok_or_else(invalid_data)
}

pub(crate) fn declaration() -> &'static str {
    ",@culprit_log_database_id int=NULL,@culprit_log_group_id int=NULL,@culprit_log_workload_group bigint=NULL,@culprit_log_resource_pool bigint=NULL,@culprit_log_instance bigint=NULL"
}

pub(crate) fn assignment() -> &'static str {
    ",@culprit_log_database_id=MIN(native.configured_log_database_id),@culprit_log_group_id=MIN(native.configured_log_group_id),@culprit_log_workload_group=MIN(native.configured_log_workload_group),@culprit_log_resource_pool=MIN(native.configured_log_resource_pool),@culprit_log_instance=MIN(native.configured_log_instance)"
}

pub(crate) fn columns() -> &'static str {
    ",@culprit_log_database_id AS configured_log_database_id,@culprit_log_group_id AS configured_log_group_id,@culprit_log_workload_group AS configured_log_workload_group,@culprit_log_resource_pool AS configured_log_resource_pool,@culprit_log_instance AS configured_log_instance"
}

#[cfg(test)]
mod tests;
