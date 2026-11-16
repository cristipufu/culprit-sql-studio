//! Per-row export proves exact membership; capture completion additionally requires the entire unique roster.
use culprit_core::telemetry::*;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct CompilerAdmissionReferences {
    gateways: BTreeSet<NativeIdentity>,
    waiters: usize,
    duplicate: bool,
}
impl CompilerAdmissionReferences {
    pub fn record(&mut self, row: &OperationalObservation) {
        if row.source != culprit_core::source_id::COMPILATION_GATEWAYS {
            return;
        }
        match &row.fact {
            OperationalFact::CompilerWaiter(_) => self.waiters += 1,
            OperationalFact::CounterSet(counters) => self.duplicate |= !self.gateways.insert(counters.identity.clone()),
            _ => (),
        }
    }
    pub fn complete(&self, source: Option<&CollectionDescription>) -> bool {
        let Some(SourceObservation::FocusedCompilationAdmission { result, .. }) =
            source.map(|source| &source.observation)
        else {
            return true;
        };
        !self.duplicate
            && result.is_valid()
            && self.waiters == usize::from(result.membership == CompilationAdmissionMembership::Observed)
            && self.gateways.len() == result.gateways.len()
            && result.gateways.iter().all(|identity| self.gateways.contains(identity))
    }
}
