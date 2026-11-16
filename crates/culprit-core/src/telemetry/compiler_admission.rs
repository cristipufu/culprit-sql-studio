//! Optimizer admission is shared context. Gateway counts do not identify a request's pool or a memory holder.
use super::*;
use crate::MeasurementScope;
use std::collections::BTreeSet;

pub const MAX_COMPILER_GATEWAYS: usize = 512;
pub const MAX_FOCUSED_COMPILER_GATEWAYS: usize = 63;
const GATEWAY_OBJECT: &str = "sys.dm_exec_query_optimizer_memory_gateways";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilerAdmissionSnapshot {
    pub source: String,
    /// None when the existing reader acquired neither native clocks nor member authority. Host clocks and a
    /// separately pooled probe cannot fill this field or qualify a relationship to another read.
    pub context: Option<NativeReadContext>,
    pub observations: Vec<OperationalObservation>,
}

impl CompilerAdmissionSnapshot {
    pub fn is_valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.context.as_ref().is_none_or(NativeReadContext::is_valid)
            && self.observations.len() <= MAX_COMPILER_GATEWAYS + 1
            && self.observations.iter().all(|row| {
                row.source == self.source
                    && row.is_valid()
                    && keys.insert(&row.key)
                    && match &row.fact {
                        OperationalFact::CounterSet(_) => CompilerGatewayFacts::from_observation(row).is_ok(),
                        OperationalFact::CompilerWaiter(waiter) => self.context.as_ref().is_some_and(|context| {
                            ObservationBounds::new(&context.observed_from, &context.observed_until)
                                .is_some_and(|bounds| bounds.contains(&waiter.observed_at))
                                && context.target.database().is_none_or(|database| {
                                    waiter.request.database_context_is_consistent(&database.database)
                                })
                        }),
                        _ => false,
                    }
            })
            && self.observations.iter().filter(|row| matches!(row.fact, OperationalFact::CompilerWaiter(_))).count()
                <= 1
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CompilerWaiterObservation {
    pub observed_at: String,
    /// Affected participant only. Its statement did not acquire another compiler's memory.
    pub request: ActivityObservation,
}

impl CompilerWaiterObservation {
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && self.request.is_valid()
            && self.request.key.request.is_some()
            && self.request.scope == ActivityScopeBasis::RequestDatabase
            && self
                .request
                .wait
                .as_ref()
                .is_some_and(|wait| wait.class == Some(ActivityWaitClass::CompilationAdmission))
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&("compiler-waiter-v1", &self.request.key))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
}

/// Interpretation of the exact source-issued native schema, not a heuristic over generic counter names.
pub struct CompilerGatewayFacts<'a> {
    pub identity: &'a NativeIdentity,
    pub resource_pool_id: &'a str,
    pub maximum_compilers: u64,
    pub active_compilers: u64,
    pub waiting_compilers: u64,
    /// None preserves native -1: passing this gate is not required.
    pub threshold_bytes: Option<u64>,
    pub threshold_factor: u64,
    pub required: bool,
}

impl<'a> CompilerGatewayFacts<'a> {
    pub fn from_observation(row: &'a OperationalObservation) -> Result<Self> {
        let OperationalFact::CounterSet(counters) = &row.fact else { return Err(invalid()) };
        if row.source != crate::source_id::COMPILATION_GATEWAYS
            || !counters.is_valid()
            || counters.key()? != row.key
            || counters.scope != MeasurementScope::Server
            || counters.identity.qualifiers.len() != 2
            || counters.identity.qualifiers.get("native_object").map(String::as_str) != Some(GATEWAY_OBJECT)
            || counters.identity.qualifiers.get("resource_pool_id").and_then(|value| integer(value)).is_none()
            || counters.properties.len() != 3
        {
            return Err(invalid());
        }
        let threshold = counters.properties.get("threshold").ok_or_else(invalid)?;
        let threshold_bytes = if threshold == "-1" { None } else { Some(integer(threshold).ok_or_else(invalid)?) };
        let required = match counters.properties.get("is_active").map(String::as_str) {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(invalid()),
        };
        let expected = 3 + usize::from(threshold_bytes.is_some());
        if counters.counters.len() != expected {
            return Err(invalid());
        }
        let facts = Self {
            identity: &counters.identity,
            resource_pool_id: counters.identity.qualifiers.get("resource_pool_id").ok_or_else(invalid)?,
            maximum_compilers: scalar(counters, "maximum-compilers", "max_count", CounterUnit::Count)?,
            active_compilers: scalar(counters, "active-compilers", "active_count", CounterUnit::Count)?,
            waiting_compilers: scalar(counters, "waiting-compilers", "waiter_count", CounterUnit::Count)?,
            threshold_bytes,
            threshold_factor: counters
                .properties
                .get("threshold_factor")
                .and_then(|value| integer(value))
                .ok_or_else(invalid)?,
            required,
        };
        if let Some(threshold) = threshold_bytes
            && scalar(counters, "admission-threshold-bytes", "threshold", CounterUnit::Bytes)? != threshold
        {
            return Err(invalid());
        }
        Ok(facts)
    }
}

fn scalar(counters: &NativeCounterSet, key: &str, native_name: &str, unit: CounterUnit) -> Result<u64> {
    let sample = counters.counters.iter().find(|sample| sample.key == key).ok_or_else(invalid)?;
    if sample.nature != CounterNature::Gauge
        || sample.unit != unit
        || sample.scope != MeasurementScope::Server
        || sample.reset != crate::ResetMarker::Unknown
        || sample.native_object != GATEWAY_OBJECT
        || sample.native_name != native_name
        || sample.native_instance.is_some()
        || sample.native_type.is_some()
    {
        return Err(invalid());
    }
    let MeasurementValue::Integer(value) = &sample.value else { return Err(invalid()) };
    integer(value).ok_or_else(invalid)
}

fn integer(value: &str) -> Option<u64> {
    let integer = value.parse::<u64>().ok()?;
    (integer.to_string() == value).then_some(integer)
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
pub(super) mod tests;
