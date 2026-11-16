//! Individual row admission cannot prove that the original subject and the entire declared peer roster arrived.
use culprit_core::telemetry::{
    CollectionDescription, ExecutionGrantObservation, MAX_REQUEST_MEMORY_OBSERVATIONS, NativeIdentity,
    SourceObservation, canonical_time,
};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct GrantReferences {
    identities: BTreeSet<NativeIdentity>,
    point: Option<String>,
    source: Option<String>,
    invalid: bool,
}

#[cfg(test)]
mod tests;
impl GrantReferences {
    pub fn record(&mut self, row: &ExecutionGrantObservation) {
        let Ok(point) = canonical_time(&row.observed_at) else {
            self.invalid = true;
            return;
        };
        if self.identities.len() >= MAX_REQUEST_MEMORY_OBSERVATIONS
            || !self.identities.insert(row.identity.clone())
            || self.point.as_ref().is_some_and(|previous| previous != &point)
            || self.source.as_ref().is_some_and(|previous| previous != &row.source)
        {
            self.invalid = true;
        }
        self.point = Some(point);
        self.source = Some(row.source.clone());
    }

    pub fn complete(&self, source: Option<&CollectionDescription>) -> bool {
        let Some(SourceObservation::FocusedRequestMemory { result, snapshot }) =
            source.map(|source| &source.observation)
        else {
            return true;
        };
        !self.invalid
            && result.is_valid()
            && self.identities.len() == snapshot.records as usize
            && self.identities.len() == result.peers.len() + usize::from(result.subject.is_some())
            && result.subject.as_ref().is_none_or(|identity| self.identities.contains(identity))
            && result.peers.iter().all(|identity| self.identities.contains(identity))
    }
}
