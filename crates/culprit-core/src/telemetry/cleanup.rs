//! Each native cleanup source retains its own acquisition bounds; separate reads are never an atomic owner join.
use super::*;
use std::collections::BTreeSet;

pub const MAX_CLEANUP_OBSERVATIONS: usize = 10_000;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CleanupSnapshot {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    pub observations: Vec<OperationalObservation>,
}

impl CleanupSnapshot {
    pub fn is_valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && self.observations.len() <= MAX_CLEANUP_OBSERVATIONS
            && self.observations.iter().all(|row| {
                row.source == self.source
                    && row.is_valid()
                    && matches!(
                        &row.fact,
                        OperationalFact::MssqlCleanupHorizon(_) | OperationalFact::MssqlPersistentVersionStore(_)
                    )
                    && keys.insert(&row.key)
            })
    }
}

#[cfg(test)]
mod tests;
