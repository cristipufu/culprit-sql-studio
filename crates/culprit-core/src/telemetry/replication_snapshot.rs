//! A completed replication read preserves native facts, source visibility and the authority of its clocks.
use super::*;
use std::{collections::BTreeMap, time::SystemTime};

pub const MAX_REPLICATION_OBSERVATIONS: usize = CollectionBudget::upper_limits().max_snapshot_rows as usize;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ReplicationReadContext {
    /// The original source descriptor supplies these native read bounds and its independent scope proof.
    Native { context: Box<NativeReadContext> },
    /// Collector-clock read bounds do not establish native event time, member continuity or connection shutdown.
    Collector { observed_from: String, observed_until: String },
}

impl ReplicationReadContext {
    pub fn collector(from: SystemTime, until: SystemTime) -> Result<Self> {
        let context = Self::Collector { observed_from: collector_time(from)?, observed_until: collector_time(until)? };
        context.is_valid().then_some(context).ok_or_else(invalid)
    }

    pub fn bounds(&self) -> (&str, &str) {
        match self {
            Self::Native { context } => (&context.observed_from, &context.observed_until),
            Self::Collector { observed_from, observed_until } => (observed_from, observed_until),
        }
    }

    pub fn native(&self) -> Option<&NativeReadContext> {
        match self {
            Self::Native { context } => Some(context),
            Self::Collector { .. } => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        let (from, until) = self.bounds();
        from.len() <= 64
            && until.len() <= 64
            && ObservationBounds::new(from, until).is_some()
            && self.native().is_none_or(NativeReadContext::is_valid)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ReplicationSnapshot {
    pub context: ReplicationReadContext,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<ReplicationObservation>,
}

impl ReplicationSnapshot {
    pub fn from_source(source: &CollectionDescription, observations: Vec<ReplicationObservation>) -> Result<Self> {
        let native = source.observation.snapshot().ok_or_else(invalid)?;
        if native.dataset != SnapshotDataset::Replication || !source.partition_is_consistent() {
            return Err(invalid());
        }
        let snapshot = Self {
            context: ReplicationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) },
            sources: native.sources.clone(),
            observations,
        };
        snapshot.is_valid_read(false).then_some(snapshot).ok_or_else(invalid)
    }

    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    /// Missing warehouse rows preserve original native counts and visibility; they do not establish absence.
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        if !self.context.is_valid()
            || self.sources.is_empty()
            || self.sources.len() > 128
            || self.observations.len() > MAX_REPLICATION_OBSERVATIONS
        {
            return false;
        }
        let mut sources = BTreeMap::new();
        let mut declared = 0u64;
        for source in &self.sources {
            if !source.is_valid() || sources.insert(source.source.as_str(), (source, 0usize)).is_some() {
                return false;
            }
            if self.context.native().is_some_and(|context| !context.partition.is_qualified()) && source.complete_scope {
                return false;
            }
            declared += u64::from(source.records);
        }
        if declared > MAX_REPLICATION_OBSERVATIONS as u64 {
            return false;
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in &self.observations {
            let Some((source, observed)) = sources.get_mut(row.source.as_str()) else { return false };
            if !row.is_valid()
                || !matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                || !keys.insert((&row.source, row.fact.key()))
            {
                return false;
            }
            *observed += 1;
        }
        sources.values().all(|(source, observed)| {
            *observed <= source.records as usize && (!read_complete || *observed == source.records as usize)
        })
    }
}

fn collector_time(value: SystemTime) -> Result<String> {
    let elapsed = value.duration_since(SystemTime::UNIX_EPOCH).map_err(|_| invalid())?;
    let seconds = i64::try_from(elapsed.as_secs()).map_err(|_| invalid())?;
    let value = chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, elapsed.subsec_nanos()).ok_or_else(invalid)?;
    Ok(value.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
