//! Historical Activity inputs remain separate; a later closure never supplies missing original authority.
use culprit_core::{BlockingReport, Result, telemetry::*};

#[derive(Debug)]
pub struct LocalActivityRead {
    pub(super) connection: String,
    pub(super) source: CollectionDescription,
    pub(super) capture: DiagnosticCaptureRef,
    pub(super) rows: Vec<LocalActivityRow>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
#[derive(Debug)]
pub(super) struct LocalActivityRow {
    pub observation: Box<ActivityObservation>,
    pub reference: DiagnosticEvidenceRef,
}
impl LocalActivityRead {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.capture
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
    pub fn frame(&self) -> Result<ActivityFrameEvidence<'_>> {
        let rows = self
            .rows
            .iter()
            .map(|row| ActivityFrameRow { observation: &row.observation, reference: Some(&row.reference) })
            .collect::<Vec<_>>();
        ActivityFrameEvidence::new(&self.source, &rows, true)
    }
}

/// Private construction requires both canonical original captures in the same closed read operation.
#[derive(Debug)]
pub struct LocalBlockingAnalysis {
    pub(super) original: LocalActivityRead,
    pub(super) followup: LocalActivityRead,
    pub(super) report: Box<BlockingReport>,
    pub(super) bytes_read: u64,
}
impl LocalBlockingAnalysis {
    pub fn original(&self) -> &LocalActivityRead {
        &self.original
    }
    pub fn followup(&self) -> &LocalActivityRead {
        &self.followup
    }
    pub fn report(&self) -> &BlockingReport {
        &self.report
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
