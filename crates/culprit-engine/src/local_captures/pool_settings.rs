//! A local Settings original retains its stream, native membership and common immutable row receipts.
use culprit_core::{PoolSettingsReport, Result, telemetry::*};
#[derive(Debug)]
pub struct LocalPoolSettings {
    pub(super) connection: String,
    pub(super) read: Box<CapturedPoolSettingsRead>,
    pub(super) report: Box<PoolSettingsReport>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalPoolSettings {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &CapturedPoolSettingsRead {
        &self.read
    }
    pub fn into_read(self) -> Box<CapturedPoolSettingsRead> {
        self.read
    }
    pub fn report(&self) -> &PoolSettingsReport {
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
    pub fn reference(&self, name: &str) -> Result<DiagnosticEvidenceRef> {
        let index = self
            .read
            .observations
            .iter()
            .position(|row| row.name == name)
            .ok_or_else(|| DeliveryProblem::Integrity.error())?;
        self.read.captured_references()?.get(index).cloned().ok_or_else(|| DeliveryProblem::Integrity.error())
    }
}
