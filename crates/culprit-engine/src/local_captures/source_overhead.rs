//! Only the authorized manifest inspection owner constructs local sanitized acquisition metadata.
use culprit_core::{
    CapturedSourceAcquisition, HistoricalSourceOverhead, RecordedSourceAcquisition, Result, SourceOverheadReport,
    telemetry::DeliveryProblem,
};
#[derive(Debug)]
pub struct LocalSourceOverhead {
    pub(super) connection: String,
    pub(super) original: RecordedSourceAcquisition,
    pub(super) report: HistoricalSourceOverhead,
    pub(super) bytes_read: u64,
}
impl LocalSourceOverhead {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn report(&self) -> &HistoricalSourceOverhead {
        &self.report
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub fn original(&self) -> Option<&CapturedSourceAcquisition> {
        match &self.original {
            RecordedSourceAcquisition::Captured { original } => Some(original),
            _ => None,
        }
    }
    /// Provisional metadata is directly inspectable, but cannot enter immutable saved history.
    pub(crate) fn into_saved(self) -> Result<RecordedSourceAcquisition> {
        if let RecordedSourceAcquisition::Captured { original } = &self.original
            && !original.is_saveable()
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(self.original)
    }
    pub fn measurements(&self) -> Option<&SourceOverheadReport> {
        match &self.report {
            HistoricalSourceOverhead::Available { report, .. } => Some(report),
            _ => None,
        }
    }
}
