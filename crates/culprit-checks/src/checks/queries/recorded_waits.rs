//! A native lock total is an observation, not a causal assignment to the statement's slow calls.
use culprit_core::{QueryStat, Snapshot, WorkloadKind, telemetry::WaitClass};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum LockTotal {
    Measured(f64),
    Observed(f64),
}
impl LockTotal {
    pub fn milliseconds(self) -> f64 {
        match self {
            Self::Measured(ms) | Self::Observed(ms) => ms,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitSource {
    QueryStore,
    Engine,
}
impl WaitSource {
    pub(super) fn of(snapshot: &Snapshot) -> Self {
        match snapshot.workload.as_ref().and_then(|workload| workload.kind()) {
            Some(WorkloadKind::QueryStore) => Self::QueryStore,
            _ => Self::Engine,
        }
    }

    pub fn label(self, snapshot: &Snapshot) -> String {
        match self {
            Self::QueryStore => culprit_core::t!("provider.mssql.source.query_store"),
            Self::Engine => snapshot.server.name().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RecordedLocks {
    pub source: WaitSource,
    pub total: LockTotal,
}
impl RecordedLocks {
    pub fn read(snapshot: &Snapshot, query: &QueryStat) -> Option<Self> {
        let total = if let Some(ms) = query.measured_wait_ms(WaitClass::Lock) {
            LockTotal::Measured(ms)
        } else {
            LockTotal::Observed(query.observed_wait_ms(WaitClass::Lock).filter(|ms| *ms > 0.0)?)
        };
        Some(Self { source: WaitSource::of(snapshot), total })
    }
}

#[cfg(test)]
mod tests;
