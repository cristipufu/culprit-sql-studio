//! One native process CPU point. Process lifetime, acquisition clock and CPU capacity are separate facts.
use super::*;

pub const MSSQL_PROCESS_CPU_OBJECT: &str = "sys.dm_os_sys_info";
pub const MSSQL_PROCESS_CPU_KIND: &str = "sql-server-process-cpu";
pub const MSSQL_PROCESS_CPU_COUNTERS: [&str; 2] = ["process_user_time_ms", "process_kernel_time_ms"];
pub const MAX_MSSQL_PROCESSOR_OBSERVATIONS: u32 = 4;

pub struct MssqlProcessCpuPoint<'a> {
    sample: &'a NativeCounterSet,
    user_ms: Option<&'a str>,
    kernel_ms: Option<&'a str>,
}

impl<'a> MssqlProcessCpuPoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        if !sample.is_valid()
            || sample.scope != crate::MeasurementScope::Server
            || sample.identity.qualifiers.len() != 4
            || sample.identity.qualifiers.get("kind").map(String::as_str) != Some(MSSQL_PROCESS_CPU_KIND)
            || sample.identity.qualifiers.get("native_object").map(String::as_str) != Some(MSSQL_PROCESS_CPU_OBJECT)
            || sample.identity.qualifiers.get("instance_id").is_none_or(|value| !valid_digest(value))
            || sample.identity.qualifiers.get("instance_epoch").is_none_or(|value| !valid_digest(value))
            || native(&sample.identity.id)? == 0
            || sample.properties.len() != 3
            || !sample.ratios.is_empty()
            || !sample.ratio_gaps.is_empty()
        {
            return Err(invalid());
        }
        let property = |name| sample.properties.get(name).map(String::as_str).ok_or_else(invalid);
        if native(property("started_at_ticks")?)? > native(property("sample_ms_ticks")?)?
            || canonical_time(property("observed_at")?).is_err()
        {
            return Err(invalid());
        }
        let mut values = std::collections::BTreeMap::new();
        for counter in &sample.counters {
            let MeasurementValue::Integer(value) = &counter.value else { return Err(invalid()) };
            native(value)?;
            if !MSSQL_PROCESS_CPU_COUNTERS.contains(&counter.key.as_str())
                || counter.native_name != counter.key
                || counter.native_object != MSSQL_PROCESS_CPU_OBJECT
                || counter.unit != CounterUnit::Milliseconds
                || counter.nature != CounterNature::Cumulative
                || counter.reset != crate::ResetMarker::Generation(sample.identity.qualifiers["instance_epoch"].clone())
                || counter.native_instance.is_some()
                || counter.native_type.is_some()
                || values.insert(counter.key.as_str(), value.as_str()).is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            sample,
            user_ms: values.get(MSSQL_PROCESS_CPU_COUNTERS[0]).copied(),
            kernel_ms: values.get(MSSQL_PROCESS_CPU_COUNTERS[1]).copied(),
        })
    }

    pub fn from_original(sample: &'a NativeCounterSet, context: &NativeReadContext) -> Result<Self> {
        let point = Self::from_sample(sample)?;
        if !context.contains_instance_point(
            crate::Engine::Mssql,
            &sample.identity.qualifiers["instance_id"],
            &sample.identity.qualifiers["instance_epoch"],
            point.observed_at(),
        ) {
            return Err(invalid());
        }
        Ok(point)
    }

    pub fn process_id(&self) -> &'a str {
        &self.sample.identity.id
    }
    pub fn observed_at(&self) -> &'a str {
        &self.sample.properties["observed_at"]
    }
    pub fn started_ticks(&self) -> &'a str {
        &self.sample.properties["started_at_ticks"]
    }
    pub fn sampled_ticks(&self) -> &'a str {
        &self.sample.properties["sample_ms_ticks"]
    }
    pub fn user_ms(&self) -> Option<&'a str> {
        self.user_ms
    }
    pub fn kernel_ms(&self) -> Option<&'a str> {
        self.kernel_ms
    }
}

fn native(value: &str) -> Result<u64> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .filter(|number| number.to_string() == value)
        .map(|value| value as u64)
        .ok_or_else(invalid)
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
