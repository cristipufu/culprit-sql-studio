//! Configuration limits are independent native scopes, not throughput or evidence of current throttling.
use super::*;

pub const CONFIGURED_LOG_RATES_SOURCE: &str = "configured-log-rates";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ConfiguredLogRateLimits {
    pub observed_at: Option<String>,
    pub database_id: String,
    /// Original logical database incarnation; an endpoint or a local numeric ID cannot replace it.
    pub database_incarnation: String,
    pub workload_group_id: Option<String>,
    pub workload_group_bytes_per_second: Option<String>,
    pub resource_pool_bytes_per_second: Option<String>,
    pub instance_bytes_per_second: Option<String>,
}

impl ConfiguredLogRateLimits {
    pub fn is_valid(&self) -> bool {
        self.observed_at.as_deref().is_none_or(|at| canonical_time(at).is_ok_and(|time| time == at))
            && native_integer(&self.database_id, i32::MAX as u64).is_some_and(|id| id > 0)
            && !self.database_incarnation.is_empty()
            && self.database_incarnation.len() <= 256
            && !self.database_incarnation.chars().any(char::is_control)
            && self.workload_group_id.as_deref().is_none_or(|id| native_integer(id, i32::MAX as u64).is_some())
            && [
                &self.workload_group_bytes_per_second,
                &self.resource_pool_bytes_per_second,
                &self.instance_bytes_per_second,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(|value| native_integer(value, i64::MAX as u64).is_some()))
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.database_incarnation, &self.database_id, &self.observed_at))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }

    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.is_valid()
            && context
                .target
                .database()
                .is_some_and(|database| database.incarnation.as_deref() == Some(self.database_incarnation.as_str()))
            && self.observed_at.as_deref().is_none_or(|at| {
                ObservationBounds::new(&context.observed_from, &context.observed_until)
                    .is_some_and(|bounds| bounds.contains(at))
            })
    }

    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        source.observation.snapshot().is_some_and(|snapshot| snapshot.dataset == SnapshotDataset::Operational)
            && NativeReadContext::from_snapshot(source).is_ok_and(|context| self.matches_context(&context))
    }
}

/// Exactly one current configuration point. Missing archived body retains the original source count.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ConfiguredLogRateRead {
    pub context: NativeReadContext,
    pub source: StateSourceCoverage,
    pub observation: Option<Box<ConfiguredLogRateLimits>>,
}

impl ConfiguredLogRateRead {
    pub fn from_source(source: &CollectionDescription, observation: Option<ConfiguredLogRateLimits>) -> Result<Self> {
        let snapshot = source.observation.snapshot().ok_or_else(invalid)?;
        if snapshot.dataset != SnapshotDataset::Operational {
            return Err(invalid());
        }
        let declared =
            snapshot.sources.iter().find(|source| source.source == CONFIGURED_LOG_RATES_SOURCE).ok_or_else(invalid)?;
        let read = Self {
            context: NativeReadContext::from_snapshot(source)?,
            source: declared.clone(),
            observation: observation.map(Box::new),
        };
        read.is_valid_read(false).then_some(read).ok_or_else(invalid)
    }

    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let observed = self.observation.is_some() as u32;
        self.context.is_valid()
            && self.context.target.database().is_some()
            && self.source.is_valid()
            && self.source.source == CONFIGURED_LOG_RATES_SOURCE
            && self.source.records <= 1
            && observed <= self.source.records
            && (!read_complete || observed == self.source.records)
            && self.observation.as_deref().is_none_or(|point| {
                matches!(self.source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                    && point.matches_context(&self.context)
            })
            && (self.context.partition.is_qualified() || !self.source.complete_scope)
    }
}

fn native_integer(value: &str, maximum: u64) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| *number <= maximum && number.to_string() == value)
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
