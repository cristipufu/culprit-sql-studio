//! An optional local-original reader is independent of native providers and warehouse adapters.
use culprit_core::{
    Result, RuntimePopulationMembership, limits::OperationHandle, page_server_read_report::PageServerReadReport,
};
use std::{future::Future, pin::Pin, time::Duration};

#[cfg(feature = "collector")]
mod outbox;
#[cfg(feature = "collector")]
pub use outbox::OutboxCaptureReader;

/// The host delegates one saved connection; the reader independently checks the original journal association.
/// The operation begins before lookup/queueing and is never replaced after work starts.
pub struct LocalCaptureRequest<'a> {
    pub connection: &'a str,
    pub observation_id: &'a str,
    pub operation: OperationHandle,
}

pub type LocalCaptureTask<'a> = Pin<Box<dyn Future<Output = Result<LocalPageServerReads>> + Send + 'a>>;

/// The default host has no local-capture capability. Installing this interface grants no native source rights.
pub trait LocalCaptureReader: Send + Sync {
    fn timeout(&self) -> Duration;
    fn page_server_reads<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a>;
}

/// Fresh reanalysis of canonical local originals, never deserialized from a report cache or a warehouse wrapper.
/// Private construction keeps the raw read/projection boundary inside the original-capture owner.
#[derive(Debug)]
pub struct LocalPageServerReads {
    report: Box<PageServerReadReport>,
    history_from: String,
    history_until: String,
    population_membership: Option<RuntimePopulationMembership>,
    collected_from: String,
    collected_until: String,
    bytes_read: u64,
}
impl LocalPageServerReads {
    pub fn report(&self) -> &PageServerReadReport {
        &self.report
    }
    /// The original native history window, not elapsed collection or verification work.
    pub fn history_from(&self) -> &str {
        &self.history_from
    }
    pub fn history_until(&self) -> &str {
        &self.history_until
    }
    /// Original source semantics only; absent metadata establishes no complete native catalog.
    pub fn population_membership(&self) -> Option<&RuntimePopulationMembership> {
        self.population_membership.as_ref()
    }
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    /// Local manifest/artifact bytes actually verified. This is neither native I/O nor source CPU work.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
