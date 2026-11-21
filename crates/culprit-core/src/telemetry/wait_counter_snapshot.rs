//! Exact wait inputs retain acquisition authority and native source coverage independently of body visibility.
use super::*;
use std::collections::BTreeMap;

const MAX_WAIT_COUNTER_SOURCES: usize = 128;

pub const MAX_WAIT_COUNTER_OBSERVATIONS: usize = CollectionBudget::upper_limits().max_snapshot_rows as usize;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WaitCounterSnapshot {
    #[serde(deserialize_with = "Option::deserialize")]
    pub pool_current: Option<Box<MssqlPoolCurrentAuthority>>,
    /// A client acquisition window is not a native counter clock, reset or physical-member proof.
    pub context: Option<ObservationReadContext>,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<WaitCounterObservation>,
    /// Body availability, separate from original native population/visibility and presentation limits.
    pub read_complete: bool,
}
impl WaitCounterSnapshot {
    pub fn from_source(
        source: &CollectionDescription,
        observations: Vec<WaitCounterObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let snapshot = source.observation.snapshot().ok_or_else(invalid)?;
        if snapshot.dataset != SnapshotDataset::WaitCounters || !source.partition_is_consistent() {
            return Err(invalid());
        }
        let read = Self {
            pool_current: mssql_pool_current_authority(source)?.cloned().map(Box::new),
            context: Some(ObservationReadContext::Native {
                context: Box::new(NativeReadContext::from_snapshot(source)?),
            }),
            sources: snapshot.sources.clone(),
            observations,
            read_complete,
        };
        (read.is_valid_for(source.target.engine()) && read.observations.iter().all(|row| row.fits_source(source)))
            .then_some(read)
            .ok_or_else(invalid)
    }

    pub fn is_valid(&self) -> bool {
        if self.context.as_ref().is_some_and(|context| !context.is_valid())
            || self.sources.is_empty()
            || self.sources.len() > MAX_WAIT_COUNTER_SOURCES
            || self.observations.len() > MAX_WAIT_COUNTER_OBSERVATIONS
        {
            return false;
        }
        let pool_target = self.context.as_ref().and_then(ObservationReadContext::native).is_some_and(|context|
            matches!(&context.target, CollectionTarget::Shared(pool) if pool.engine == crate::Engine::Mssql && pool.kind == SharedResourceKind::Pool));
        if pool_target != self.pool_current.is_some()
            || self.pool_current.as_ref().is_some_and(|authority| {
                !authority.accepts_sources(&self.sources)
                    || !self
                        .context
                        .as_ref()
                        .and_then(ObservationReadContext::native)
                        .is_some_and(|context| authority.matches_context(context))
                    || authority.before.scope_qualification() != MssqlPoolScopeQualification::Qualified
                        && !self.observations.is_empty()
                    || self.observations.iter().any(|row| {
                        row.source != crate::source_id::WAITS
                            || row.scope != crate::MeasurementScope::Pool
                            || row.reset != crate::ResetMarker::Unknown
                    })
            })
        {
            return false;
        }
        let mut sources = BTreeMap::new();
        let mut native_rows = 0u64;
        for coverage in &self.sources {
            if !coverage.is_valid() || sources.insert(coverage.source.as_str(), (coverage, 0usize)).is_some() {
                return false;
            }
            native_rows += u64::from(coverage.records);
            if self
                .context
                .as_ref()
                .and_then(ObservationReadContext::native)
                .is_some_and(|context| !context.partition.is_qualified())
                && coverage.complete_scope
            {
                return false;
            }
        }
        if native_rows > MAX_WAIT_COUNTER_OBSERVATIONS as u64 {
            return false;
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in &self.observations {
            let Some((coverage, retained)) = sources.get_mut(row.source.as_str()) else { return false };
            // This is exactly NativeRecordIdentity::WaitCounter. Scope/reset changes cannot create a second row.
            if !row.is_valid()
                || !keys.insert((&row.source, &row.wait_type))
                || !matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
            {
                return false;
            }
            *retained += 1;
        }
        sources.values().all(|(coverage, retained)| {
            *retained <= coverage.records as usize && (!self.read_complete || *retained == coverage.records as usize)
        })
    }

    pub fn is_valid_for(&self, engine: crate::Engine) -> bool {
        self.is_valid()
            && (engine != crate::Engine::Mssql
                || !self.observations.iter().any(|row| row.scope == crate::MeasurementScope::Pool)
                || self.pool_current.is_some())
            && self
                .context
                .as_ref()
                .and_then(ObservationReadContext::native)
                .is_none_or(|context| context.target.engine() == engine)
            && self.observations.iter().all(|row| row.is_valid_for(engine))
    }
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
