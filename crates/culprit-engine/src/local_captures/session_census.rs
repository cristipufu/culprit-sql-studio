use super::LocalActivityRead;
use culprit_core::SessionCensusReport;

/// The same opaque original Activity read owns both the native rows and their fresh informational view.
#[derive(Debug)]
pub struct LocalSessionCensus {
    pub(super) original: LocalActivityRead,
    pub(super) report: Box<SessionCensusReport>,
}
impl LocalSessionCensus {
    pub fn original(&self) -> &LocalActivityRead {
        &self.original
    }
    pub fn report(&self) -> &SessionCensusReport {
        &self.report
    }
}
