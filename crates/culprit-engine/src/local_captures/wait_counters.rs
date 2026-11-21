//! Original wait rows retain the generic snapshot verifier and never become database runtime attribution.
use culprit_core::{Result, WaitCounterReport, telemetry::*};
#[derive(Debug)]
pub struct LocalWaitCounters {
    pub(super) read: Box<WaitCounterSnapshot>,
    pub(super) report: Box<WaitCounterReport>,
    pub(super) references: Vec<DiagnosticEvidenceRef>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalWaitCounters {
    pub fn read(&self) -> &WaitCounterSnapshot {
        &self.read
    }
    pub fn report(&self) -> &WaitCounterReport {
        &self.report
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
    pub fn reference(&self, source: &str, wait_type: &str) -> Result<&DiagnosticEvidenceRef> {
        self.read
            .observations
            .iter()
            .position(|row| row.source == source && row.wait_type == wait_type)
            .and_then(|index| self.references.get(index))
            .ok_or_else(|| DeliveryProblem::Integrity.error())
    }
}
