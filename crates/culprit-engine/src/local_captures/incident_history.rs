//! Read-only history of explicitly selected immutable local revisions; it is never a controller restoration.
use culprit_core::{IncidentHistoryReport, telemetry::DiagnosticCaptureRef};
#[derive(Debug)]
pub struct LocalIncidentHistory {
    pub(super) connection: String,
    pub(super) original: DiagnosticCaptureRef,
    pub(super) report: Box<IncidentHistoryReport>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalIncidentHistory {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn original(&self) -> &DiagnosticCaptureRef {
        &self.original
    }
    pub fn report(&self) -> &IncidentHistoryReport {
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
}
