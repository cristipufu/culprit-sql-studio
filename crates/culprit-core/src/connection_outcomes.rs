//! Native connection and session counters retain their source meaning, population and independent reset proof.
use crate::{MeasurementScope, ResetMarker, Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

pub const MAX_CONNECTION_OUTCOME_ROWS: usize = CollectionBudget::upper_limits().max_snapshot_rows as usize;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ConnectionOutcomeMetricKind {
    SessionsEstablished,
    SessionsLostClient,
    SessionsFatal,
    SessionsOperatorTerminated,
    SessionTime,
    ActiveTime,
    IdleInTransactionTime,
    ConnectionAttempts,
    FailedConnectionAttempts,
    AbortedClientConnections,
    ConnectionErrorsAccept,
    ConnectionErrorsInternal,
    ConnectionErrorsMaxConnections,
    ConnectionErrorsPeerAddress,
    ConnectionErrorsSelect,
    ConnectionErrorsTcpwrap,
    ThreadsCreated,
    ThreadsConnected,
    ThreadsRunning,
}

impl ConnectionOutcomeMetricKind {
    pub fn unit(self) -> CounterUnit {
        match self {
            Self::SessionTime | Self::ActiveTime | Self::IdleInTransactionTime => CounterUnit::Milliseconds,
            _ => CounterUnit::Count,
        }
    }

    pub fn nature(self) -> CounterNature {
        match self {
            Self::ThreadsConnected | Self::ThreadsRunning => CounterNature::Gauge,
            _ => CounterNature::Cumulative,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ConnectionOutcomeAvailability {
    Supported,
    Unsupported,
    Unknown,
}

/// An engine's source policy maps its own retained native schema to a precise meaning. A consumer never guesses
/// from a raw name, another engine's similarly named counter, a SQL dialect or a connection endpoint.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionOutcomeMetricSpec {
    pub native_key: &'static str,
    pub kind: ConnectionOutcomeMetricKind,
    pub availability: ConnectionOutcomeAvailability,
}

#[derive(Clone, Debug)]
pub struct ConnectionOutcomeProfile {
    pub scope: MeasurementScope,
    pub metrics: Vec<ConnectionOutcomeMetricSpec>,
    /// Optional native clock from the SAME counter statement. Client clocks and an earlier probe cannot fill it.
    pub observed_at: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ConnectionOutcomeObservation {
    pub source: String,
    pub counters: NativeCounterSet,
}

impl ConnectionOutcomeObservation {
    pub fn key(&self) -> Result<String> {
        self.counters.key()
    }

    pub fn from_operational(row: &OperationalObservation) -> Result<Self> {
        let OperationalFact::CounterSet(counters) = &row.fact else { return Err(invalid()) };
        if !row.is_valid() {
            return Err(invalid());
        }
        Ok(Self { source: row.source.clone(), counters: counters.as_ref().clone() })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ConnectionOutcomeSnapshot {
    /// The configured or source-owned namespace. It does not attribute server-wide counters to this database.
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub sources: Vec<StateSourceCoverage>,
    /// Preserve every original CounterSet row/count, including unrelated variables from the same native source.
    pub observations: Vec<ConnectionOutcomeObservation>,
}

impl ConnectionOutcomeSnapshot {
    pub fn from_source(source: &CollectionDescription, observations: Vec<OperationalObservation>) -> Result<Self> {
        let native = source.observation.snapshot().ok_or_else(invalid)?;
        if native.dataset != SnapshotDataset::Operational || !source.partition_is_consistent() {
            return Err(invalid());
        }
        let snapshot = Self {
            target: source.target.clone(),
            context: ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) },
            sources: native.sources.clone(),
            observations: observations
                .iter()
                .map(ConnectionOutcomeObservation::from_operational)
                .collect::<Result<_>>()?,
        };
        snapshot.is_valid_read(false).then_some(snapshot).ok_or_else(invalid)
    }

    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        if !self.target.is_valid()
            || !self.context.is_valid()
            || self.context.native().is_some_and(|context| context.target != self.target)
            || self.sources.is_empty()
            || self.sources.len() > 128
            || self.observations.len() > MAX_CONNECTION_OUTCOME_ROWS
        {
            return false;
        }
        let mut sources = BTreeMap::new();
        let mut declared = 0u64;
        for source in &self.sources {
            if !source.is_valid()
                || sources.insert(source.source.as_str(), (source, 0usize)).is_some()
                || self.context.native().is_some_and(|context| !context.partition.is_qualified())
                    && source.complete_scope
            {
                return false;
            }
            declared += u64::from(source.records);
        }
        if declared > MAX_CONNECTION_OUTCOME_ROWS as u64 {
            return false;
        }
        let mut keys = BTreeSet::new();
        for row in &self.observations {
            let Some((source, count)) = sources.get_mut(row.source.as_str()) else { return false };
            if !row.counters.is_valid()
                || !matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                || !keys.insert((&row.source, row.key().ok()))
            {
                return false;
            }
            if let Some(profile) = self.profile(row)
                && (profile.scope != row.counters.scope
                    || (profile.scope == MeasurementScope::Database
                        && self.target.database().is_none_or(|database| {
                            row.counters.properties.get("datname").is_some_and(|name| name != &database.database)
                        }))
                    || profile.metrics.is_empty()
                    || profile.metrics.len() > 32
                    || profile.observed_at.as_deref().is_some_and(|at| {
                        at.len() > 64
                            || canonical_time(at).is_err()
                            || self.context.native().is_some_and(|context| {
                                !ObservationBounds::new(&context.observed_from, &context.observed_until)
                                    .is_some_and(|bounds| bounds.contains(at))
                            })
                    })
                    || profile.metrics.iter().enumerate().any(|(index, metric)| {
                        row.counters.counters.iter().any(|counter| {
                            counter.key == metric.native_key
                                && (counter.unit != metric.kind.unit()
                                    || counter.nature != metric.kind.nature()
                                    || (metric.kind.unit() == CounterUnit::Count && counter.value.integer().is_none())
                                    || metric.availability == ConnectionOutcomeAvailability::Unsupported)
                        }) || profile.metrics[..index]
                            .iter()
                            .any(|prior| prior.kind == metric.kind || prior.native_key == metric.native_key)
                    }))
            {
                return false;
            }
            *count += 1;
        }
        sources.values().all(|(source, count)| {
            *count <= source.records as usize && (!read_complete || *count == source.records as usize)
        })
    }

    pub fn profile(&self, row: &ConnectionOutcomeObservation) -> Option<ConnectionOutcomeProfile> {
        self.target.engine().spec().connection_outcome_profile(&row.source, &row.counters)
    }

    /// One source-owned cumulative counter with a same-read native clock and explicit native reset. This is not a
    /// delta between snapshots; an identical reset time alone never establishes cross-member continuity.
    pub fn rate(&self, kind: ConnectionOutcomeMetricKind) -> Option<ConnectionOutcomeRate> {
        self.rates().remove(&kind)
    }

    /// Duplicate populations never select a first representative or pool their totals.
    pub fn rates(&self) -> BTreeMap<ConnectionOutcomeMetricKind, ConnectionOutcomeRate> {
        if !self.is_valid_read(false) {
            return BTreeMap::new();
        }
        let mut rates = BTreeMap::new();
        for row in &self.observations {
            let Some(profile) = self.profile(row) else { continue };
            for metric in &profile.metrics {
                let rate = native_rate(&profile, &row.counters, metric);
                match rates.entry(metric.kind) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(rate);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        entry.insert(None);
                    }
                }
            }
        }
        rates.into_iter().filter_map(|(kind, rate)| rate.map(|rate| (kind, rate))).collect()
    }
}

fn native_rate(
    profile: &ConnectionOutcomeProfile,
    counters: &NativeCounterSet,
    metric: &ConnectionOutcomeMetricSpec,
) -> Option<ConnectionOutcomeRate> {
    if metric.availability != ConnectionOutcomeAvailability::Supported {
        return None;
    }
    let counter = counters.counters.iter().find(|counter| counter.key == metric.native_key)?;
    if counter.nature != CounterNature::Cumulative || counter.unit != CounterUnit::Count {
        return None;
    }
    let ResetMarker::At(reset_at) = &counter.reset else { return None };
    let observed_at = profile.observed_at.as_ref()?;
    let seconds = crate::counter_rates::counter_span_seconds(&counter.reset, observed_at)?;
    let value = counter.value.integer()? as f64;
    let rate = crate::counter_rates::AdmittedCounterRate::new(value, seconds)?;
    Some(ConnectionOutcomeRate {
        per_second: rate.per_second(),
        seconds: rate.seconds(),
        reset_at: reset_at.clone(),
        observed_at: observed_at.clone(),
    })
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ConnectionOutcomeRate {
    pub per_second: f64,
    pub seconds: f64,
    pub reset_at: String,
    pub observed_at: String,
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
