//! Native named counter instances are current Resource Governor context, never database/query ownership.
use super::*;

pub const MSSQL_RESOURCE_GOVERNOR_COUNTER_KIND: &str = "sql-server-resource-governor-counters";
pub const MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE: &str = "sqlserver-resource-governor-counters-v1";
pub const MSSQL_RESOURCE_GOVERNOR_REQUEST_PROPERTY: &str = "resource_governor_requested";

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlResourceGovernorCounterKind {
    WorkloadGroup,
    ResourcePool,
}
impl MssqlResourceGovernorCounterKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::WorkloadGroup => "workload-group",
            Self::ResourcePool => "resource-pool",
        }
    }
    pub fn object_family(self) -> &'static str {
        match self {
            Self::WorkloadGroup => "Workload Group Stats",
            Self::ResourcePool => "Resource Pool Stats",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlResourceGovernorCounterContext {
    pub kind: MssqlResourceGovernorCounterKind,
    pub native_group_id: Option<String>,
    pub native_pool_id: Option<String>,
    /// Native DMV local time, independently from the performance counter's unknown reset boundary.
    pub statistics_started_without_offset: Option<String>,
}

pub fn resource_governor_counter_set(sample: &NativeCounterSet) -> bool {
    sample.identity.qualifiers.get("kind").map(String::as_str) == Some(MSSQL_RESOURCE_GOVERNOR_COUNTER_KIND)
}
pub fn resource_governor_candidate(sample: &NativeCounterSet) -> bool {
    resource_governor_counter_set(sample) || sample.properties.contains_key(MSSQL_RESOURCE_GOVERNOR_REQUEST_PROPERTY)
}

/// A requested population with no returned rows is unavailable, never an empty healthy group inventory.
pub fn resource_governor_requested_rows(sample: &NativeCounterSet) -> Result<Option<u32>> {
    let Some(requested) = sample.properties.get(MSSQL_RESOURCE_GOVERNOR_REQUEST_PROPERTY) else { return Ok(None) };
    if requested != "true" || resource_governor_counter_set(sample) {
        return Err(invalid());
    }
    observer(sample)?;
    let raw = sample.properties.get("resource_governor_native_rows").ok_or_else(invalid)?;
    let rows = raw
        .parse::<u32>()
        .ok()
        .filter(|rows| rows.to_string() == *raw && *rows as usize <= MAX_NATIVE_COUNTERS)
        .ok_or_else(invalid)?;
    Ok(Some(rows))
}

pub struct MssqlResourceGovernorCounterSet<'a> {
    sample: &'a NativeCounterSet,
    metadata: MssqlResourceGovernorCounterContext,
}
impl<'a> MssqlResourceGovernorCounterSet<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        if !sample.is_valid()
            || !resource_governor_counter_set(sample)
            || sample.identity.qualifiers.get("native_object").map(String::as_str)
                != Some("sys.dm_os_performance_counters")
            || sample.scope != crate::MeasurementScope::Server
            || !sample.ratios.is_empty()
            || !sample.ratio_gaps.is_empty()
            || sample.counters.is_empty()
            || sample.properties.get(PERFORMANCE_COUNTER_PROFILE_PROPERTY).map(String::as_str)
                != Some(MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE)
        {
            return Err(invalid());
        }
        observer(sample)?;
        let kind = match sample.identity.qualifiers.get("resource_kind").map(String::as_str) {
            Some("workload-group") => MssqlResourceGovernorCounterKind::WorkloadGroup,
            Some("resource-pool") => MssqlResourceGovernorCounterKind::ResourcePool,
            _ => return Err(invalid()),
        };
        let name = sample.properties.get("resource_name").ok_or_else(invalid)?;
        if name.is_empty() || name.encode_utf16().count() > 128 || sample.identity.id != digest(name.as_bytes()) {
            return Err(invalid());
        }
        let group = optional_id(sample, "group_id")?;
        let pool = optional_id(sample, "pool_id")?;
        let started = sample.properties.get("statistics_started_without_offset").cloned();
        if kind == MssqlResourceGovernorCounterKind::ResourcePool && group.is_some()
            || kind == MssqlResourceGovernorCounterKind::WorkloadGroup && group.is_some() != pool.is_some()
            || started.as_deref().is_some_and(|value| {
                value.len() != 27
                    || !value.is_ascii()
                    || &value[..4] < "1753"
                    || !super::super::catalog::local_time(value)
            })
            || pool.is_none() && started.is_some()
            || sample.counters.iter().any(|counter| {
                counter.native_instance.as_deref() != Some(name.as_str())
                    || !counter.native_object.ends_with(&format!(":{}", kind.object_family()))
                    || counter.reset != crate::ResetMarker::Unknown
            })
        {
            return Err(invalid());
        }
        Ok(Self {
            sample,
            metadata: MssqlResourceGovernorCounterContext {
                kind,
                native_group_id: group,
                native_pool_id: pool,
                statistics_started_without_offset: started,
            },
        })
    }
    pub fn context(&self) -> MssqlResourceGovernorCounterContext {
        self.metadata.clone()
    }
    pub fn has_native_ids(&self) -> bool {
        self.metadata.native_pool_id.is_some()
    }
    pub fn observed_at(&self) -> Result<&'a str> {
        let raw = self.sample.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY).ok_or_else(invalid)?;
        if canonical_time(raw)?.as_str() != raw.as_str() {
            return Err(invalid());
        }
        Ok(raw)
    }
}

pub fn resource_governor_fits_source(sample: &NativeCounterSet, source: &CollectionDescription) -> bool {
    source.observation.snapshot().is_some_and(|snapshot| snapshot.dataset == SnapshotDataset::Operational)
        && NativeReadContext::from_snapshot(source)
            .is_ok_and(|context| MssqlResourceGovernorCounterSet::fits_context(sample, &context))
}
impl MssqlResourceGovernorCounterSet<'_> {
    /// The request marker and each native group/pool set belong to this exact original observer and process.
    pub fn fits_context(sample: &NativeCounterSet, context: &NativeReadContext) -> bool {
        let Ok((instance, epoch)) = observer(sample) else { return false };
        sample
            .properties
            .get(PERFORMANCE_COUNTER_CLOCK_PROPERTY)
            .is_some_and(|at| context.contains_instance_point(crate::Engine::Mssql, instance, epoch, at))
    }
}
fn observer(sample: &NativeCounterSet) -> Result<(&str, &str)> {
    let properties =
        if resource_governor_counter_set(sample) { &sample.identity.qualifiers } else { &sample.properties };
    let instance = properties.get("resource_governor_instance_id").ok_or_else(invalid)?;
    let epoch = properties.get("resource_governor_instance_epoch").ok_or_else(invalid)?;
    if !valid_digest(instance) || !valid_digest(epoch) {
        return Err(invalid());
    }
    Ok((instance, epoch))
}
fn optional_id(sample: &NativeCounterSet, key: &str) -> Result<Option<String>> {
    sample
        .properties
        .get(key)
        .map(|value| {
            value
                .parse::<i32>()
                .ok()
                .filter(|id| *id > 0 && id.to_string() == *value)
                .map(|_| value.clone())
                .ok_or_else(invalid)
        })
        .transpose()
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
