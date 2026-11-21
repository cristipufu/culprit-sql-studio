//! Closed native CPU populations keep reset witnesses, capacity and retained-sample clocks distinct.
use super::*;
use crate::{Engine, MeasurementScope, ResetMarker, source_id};

pub const MSSQL_CPU_PROFILE_SOURCE: &str = "mssql-process-cpu";
pub const MSSQL_CPU_POOLS_SOURCE: &str = "mssql-engine-cpu-pools";
pub const MSSQL_CPU_HISTORY_SOURCE: &str = "mssql-process-cpu-history";
pub const MAX_MSSQL_CPU_POOLS: u32 = 64;
pub const MAX_MSSQL_CPU_OBSERVATIONS: u32 = MAX_MSSQL_PROCESSOR_OBSERVATIONS + MAX_MSSQL_CPU_POOLS + 1;
pub const MSSQL_CPU_POOL_KIND: &str = "sql-server-engine-cpu-pool";
pub const MSSQL_CPU_POOL_OBJECT: &str = "sys.dm_resource_governor_resource_pools";
pub const MSSQL_CPU_HISTORY_KIND: &str = "sql-server-process-cpu-history";
pub const MSSQL_CPU_HISTORY_OBJECT: &str = "sys.dm_os_ring_buffers";
const COMMON: [&str; 5] = ["observed_at", "started_at_ticks", "sample_ms_ticks", "engine_edition", "major"];

pub fn cpu_pool_candidate(sample: &NativeCounterSet) -> bool {
    candidate(sample, MSSQL_CPU_POOL_KIND, MSSQL_CPU_POOL_OBJECT)
}
pub fn cpu_history_candidate(sample: &NativeCounterSet) -> bool {
    candidate(sample, MSSQL_CPU_HISTORY_KIND, MSSQL_CPU_HISTORY_OBJECT)
}
fn candidate(sample: &NativeCounterSet, kind: &str, object: &str) -> bool {
    sample.identity.qualifiers.get("kind").is_some_and(|value| value == kind)
        || sample.identity.qualifiers.get("native_object").is_some_and(|value| value == object)
}

fn validate_point(
    sample: &NativeCounterSet,
    kind: &str,
    object: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<()> {
    if !sample.is_valid()
        || sample.scope != MeasurementScope::Server
        || sample.identity.qualifiers.len() != 4
        || sample.identity.qualifiers.get("kind").map(String::as_str) != Some(kind)
        || sample.identity.qualifiers.get("native_object").map(String::as_str) != Some(object)
        || sample.identity.qualifiers.get("instance_id").is_none_or(|value| !valid_digest(value))
        || sample.identity.qualifiers.get("instance_epoch").is_none_or(|value| !valid_digest(value))
        || !sample.ratios.is_empty()
        || !sample.ratio_gaps.is_empty()
        || COMMON.iter().chain(required).any(|name| !sample.properties.contains_key(*name))
        || sample.properties.keys().any(|name| {
            !COMMON.contains(&name.as_str()) && !required.contains(&name.as_str()) && !optional.contains(&name.as_str())
        })
    {
        return Err(invalid());
    }
    if !["2", "3", "4"].contains(&property(sample, "engine_edition")?)
        || native(property(sample, "major")?)? < 11
        || native(property(sample, "started_at_ticks")?)? > native(property(sample, "sample_ms_ticks")?)?
        || canonical_time(property(sample, "observed_at")?)? != property(sample, "observed_at")?
    {
        return Err(invalid());
    }
    Ok(())
}
fn property<'a>(sample: &'a NativeCounterSet, name: &str) -> Result<&'a str> {
    sample.properties.get(name).map(String::as_str).ok_or_else(invalid)
}
fn fits_point(sample: &NativeCounterSet, context: &NativeReadContext) -> bool {
    context.contains_instance_point(
        Engine::Mssql,
        &sample.identity.qualifiers["instance_id"],
        &sample.identity.qualifiers["instance_epoch"],
        &sample.properties["observed_at"],
    )
}

pub struct MssqlCpuPoolPoint<'a> {
    sample: &'a NativeCounterSet,
    total_ms: Option<&'a str>,
}
impl<'a> MssqlCpuPoolPoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        validate_point(
            sample,
            MSSQL_CPU_POOL_KIND,
            MSSQL_CPU_POOL_OBJECT,
            &["host_logical_processors", "statistics_started_without_offset"],
            &[],
        )?;
        if native(&sample.identity.id)? > i32::MAX as u64
            || !(1..=i32::MAX as u64).contains(&native(property(sample, "host_logical_processors")?)?)
            || !crate::telemetry::catalog::local_time(property(sample, "statistics_started_without_offset")?)
            || sample.counters.len() > 1
        {
            return Err(invalid());
        }
        let mut total_ms = None;
        for counter in &sample.counters {
            let MeasurementValue::Integer(value) = &counter.value else { return Err(invalid()) };
            native(value)?;
            if counter.key != "total_cpu_usage_ms"
                || counter.native_name != counter.key
                || counter.native_object != MSSQL_CPU_POOL_OBJECT
                || counter.native_instance.as_deref() != Some(sample.identity.id.as_str())
                || counter.native_type.is_some()
                || counter.unit != CounterUnit::Milliseconds
                || counter.nature != CounterNature::Cumulative
                || counter.scope != MeasurementScope::Server
                || counter.reset != ResetMarker::Unknown
            {
                return Err(invalid());
            }
            total_ms = Some(value.as_str());
        }
        Ok(Self { sample, total_ms })
    }
    pub fn from_original(sample: &'a NativeCounterSet, context: &NativeReadContext) -> Result<Self> {
        let point = Self::from_sample(sample)?;
        if !fits_point(sample, context) {
            return Err(invalid());
        }
        Ok(point)
    }
    pub fn pool_id(&self) -> &'a str {
        &self.sample.identity.id
    }
    pub fn total_ms(&self) -> Option<&'a str> {
        self.total_ms
    }
    pub fn statistics_started(&self) -> &'a str {
        &self.sample.properties["statistics_started_without_offset"]
    }
    pub fn observed_at(&self) -> &'a str {
        &self.sample.properties["observed_at"]
    }
    pub fn sample_ticks(&self) -> &'a str {
        &self.sample.properties["sample_ms_ticks"]
    }
    pub fn host_processors(&self) -> &'a str {
        &self.sample.properties["host_logical_processors"]
    }
    pub fn instance_id(&self) -> &'a str {
        &self.sample.identity.qualifiers["instance_id"]
    }
    pub fn instance_epoch(&self) -> &'a str {
        &self.sample.identity.qualifiers["instance_epoch"]
    }
}

/// Native raw fields are retained even when their platform/range cannot qualify them as percentages.
pub struct MssqlCpuHistoryPoint<'a> {
    sample: &'a NativeCounterSet,
}
impl<'a> MssqlCpuHistoryPoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        validate_point(
            sample,
            MSSQL_CPU_HISTORY_KIND,
            MSSQL_CPU_HISTORY_OBJECT,
            &["record_ms_ticks"],
            &["host_platform", "process_utilization", "system_idle"],
        )?;
        if native(&sample.identity.id)? > i32::MAX as u64 || !sample.counters.is_empty() {
            return Err(invalid());
        }
        let record = native(property(sample, "record_ms_ticks")?)?;
        if record < native(property(sample, "started_at_ticks")?)?
            || record > native(property(sample, "sample_ms_ticks")?)?
        {
            return Err(invalid());
        }
        if let Some(platform) = sample.properties.get("host_platform")
            && (platform.is_empty() || platform.len() > 32)
        {
            return Err(invalid());
        }
        for name in ["process_utilization", "system_idle"] {
            if let Some(value) = sample.properties.get(name)
                && value.parse::<i32>().ok().is_none_or(|number| number.to_string() != *value)
            {
                return Err(invalid());
            }
        }
        Ok(Self { sample })
    }
    pub fn from_original(sample: &'a NativeCounterSet, context: &NativeReadContext) -> Result<Self> {
        let point = Self::from_sample(sample)?;
        if !fits_point(sample, context) {
            return Err(invalid());
        }
        Ok(point)
    }
    pub fn observed_at(&self) -> &'a str {
        &self.sample.properties["observed_at"]
    }
    pub fn record_ticks(&self) -> &'a str {
        &self.sample.properties["record_ms_ticks"]
    }
    pub fn sample_ticks(&self) -> &'a str {
        &self.sample.properties["sample_ms_ticks"]
    }
    pub fn platform(&self) -> Option<&'a str> {
        self.sample.properties.get("host_platform").map(String::as_str)
    }
    pub fn raw_process(&self) -> Option<&'a str> {
        self.sample.properties.get("process_utilization").map(String::as_str)
    }
    pub fn raw_idle(&self) -> Option<&'a str> {
        self.sample.properties.get("system_idle").map(String::as_str)
    }
    pub fn process_percent(&self) -> Option<&'a str> {
        let major = native(&self.sample.properties["major"]).ok()?;
        match self.platform()? {
            "Windows" if major >= 14 => (),
            "Linux" if major >= 15 => (),
            _ => return None,
        }
        percent(self.raw_process()?)
    }
    pub fn idle_percent(&self) -> Option<&'a str> {
        if self.platform() != Some("Windows") {
            return None;
        }
        percent(self.raw_idle()?)
    }
}
fn percent(value: &str) -> Option<&str> {
    value.parse::<u32>().ok().filter(|value| *value <= 100).map(|_| value)
}

pub fn cpu_profile_header_is_valid(source: &CollectionDescription) -> bool {
    let Some(snapshot) = source.observation.snapshot() else { return false };
    let CollectionTarget::Shared(target) = &source.target else { return false };
    let SourcePartition::InstanceLocal { member: Some(member), epoch: Some(epoch) } = &source.partition else {
        return false;
    };
    let expected = [
        (source_id::PROCESSOR_TOPOLOGY, MAX_MSSQL_PROCESSOR_OBSERVATIONS),
        (MSSQL_CPU_POOLS_SOURCE, MAX_MSSQL_CPU_POOLS),
        (MSSQL_CPU_HISTORY_SOURCE, 1),
    ];
    snapshot.dataset == SnapshotDataset::ProcessCpu
        && source.source == MSSQL_CPU_PROFILE_SOURCE
        && snapshot.is_valid()
        && snapshot.records <= MAX_MSSQL_CPU_OBSERVATIONS
        && target.engine == Engine::Mssql
        && target.kind == SharedResourceKind::Instance
        && target == member
        && valid_digest(epoch)
        && snapshot.sources.len() == expected.len()
        && expected.iter().all(|(name, max)| {
            snapshot.sources.iter().filter(|source| source.source == *name && source.records <= *max).count() == 1
        })
}
pub fn cpu_profile_observation_fits(row: &OperationalObservation, source: &CollectionDescription) -> bool {
    if !cpu_profile_header_is_valid(source) || !row.is_valid() {
        return false;
    }
    let Ok(context) = NativeReadContext::from_snapshot(source) else { return false };
    match &row.fact {
        OperationalFact::CounterSet(sample) if cpu_pool_candidate(sample) => {
            row.source == MSSQL_CPU_POOLS_SOURCE && MssqlCpuPoolPoint::from_original(sample, &context).is_ok()
        }
        OperationalFact::CounterSet(sample) if cpu_history_candidate(sample) => {
            row.source == MSSQL_CPU_HISTORY_SOURCE && MssqlCpuHistoryPoint::from_original(sample, &context).is_ok()
        }
        OperationalFact::CounterSet(sample) => {
            row.source == source_id::PROCESSOR_TOPOLOGY && MssqlProcessCpuPoint::from_original(sample, &context).is_ok()
        }
        OperationalFact::Counter(counter) => {
            let name = match row.key.as_str() {
                "host-logical-processors" => "cpu_count",
                "visible-online-user-schedulers" => "online_schedulers",
                "visible-online-numa-nodes" => "numa_nodes",
                _ => return false,
            };
            row.source == source_id::PROCESSOR_TOPOLOGY
                && counter.key == row.key
                && counter.native_name == name
                && counter.native_object
                    == if name == "cpu_count" { "sys.dm_os_sys_info" } else { "sys.dm_os_schedulers" }
                && counter.native_instance.is_none()
                && counter.native_type.is_none()
                && counter.unit == CounterUnit::Count
                && counter.nature == CounterNature::Gauge
                && counter.scope == MeasurementScope::Server
                && counter.reset == ResetMarker::Unknown
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
