//! Authenticated ReplicationSamples remain in their native family; local analysis opens no provider.
use culprit_core::{ReplicationReference, ReplicationReport, Result, telemetry::*};
#[derive(Debug)]
pub struct LocalReplication {
    pub(super) source: CollectionDescription,
    pub(super) rows: Vec<(ReplicationObservation, DiagnosticEvidenceRef)>,
    pub(super) report: Box<ReplicationReport>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalReplication {
    pub fn report(&self) -> &ReplicationReport {
        &self.report
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.source
    }
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub fn reference(&self, reference: &ReplicationReference) -> Result<&DiagnosticEvidenceRef> {
        self.rows
            .iter()
            .find(|(row, _)| row.source == reference.source && row.fact.key() == reference.observation_key)
            .map(|(_, reference)| reference)
            .ok_or_else(|| DeliveryProblem::Integrity.error())
    }
}
