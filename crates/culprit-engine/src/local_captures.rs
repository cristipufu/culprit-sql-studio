//! An optional local-original reader is independent of native providers and warehouse adapters.
use culprit_core::{
    MemoryClerkReport, Result, RuntimePopulationMembership, limits::OperationHandle,
    page_server_read_report::PageServerReadReport, telemetry::CapturedOperationalRead,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, time::Duration};
use ts_rs::TS;

#[cfg(feature = "collector")]
mod outbox;
#[cfg(feature = "collector")]
pub use outbox::OutboxCaptureReader;

/// A closed analysis choice; selecting an analysis starts no native collection or source discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LocalCaptureAnalysisKind {
    PageServerReads,
    MemoryClerks,
    HyperscaleLogComponents,
}
impl LocalCaptureAnalysisKind {
    pub const ALL: [Self; 3] = [Self::PageServerReads, Self::MemoryClerks, Self::HyperscaleLogComponents];
}

/// The host delegates one saved connection; the reader independently checks the original journal association.
/// The operation begins before lookup/queueing and is never replaced after work starts.
pub struct LocalCaptureRequest<'a> {
    pub connection: &'a str,
    pub observation_id: &'a str,
    pub analysis: LocalCaptureAnalysisKind,
    pub operation: OperationHandle,
}

pub type LocalCaptureTask<'a> = Pin<Box<dyn Future<Output = Result<LocalCapturedAnalysis>> + Send + 'a>>;

#[derive(Debug)]
pub enum LocalCapturedAnalysis {
    PageServerReads(LocalPageServerReads),
    MemoryClerks(LocalMemoryClerks),
    HyperscaleLogComponents(LocalHyperscaleLogComponents),
}
impl LocalCapturedAnalysis {
    pub fn kind(&self) -> LocalCaptureAnalysisKind {
        match self {
            Self::PageServerReads(_) => LocalCaptureAnalysisKind::PageServerReads,
            Self::MemoryClerks(_) => LocalCaptureAnalysisKind::MemoryClerks,
            Self::HyperscaleLogComponents(_) => LocalCaptureAnalysisKind::HyperscaleLogComponents,
        }
    }
}

/// The default host has no local-capture capability. Installing this interface grants no native source rights.
pub trait LocalCaptureReader: Send + Sync {
    fn timeout(&self) -> Duration;
    fn analyze<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a>;
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

/// Original Operational rows and their fresh interpretation share one authenticated local read.
/// Shared-instance data stays independent of a selected database or its current native member.
#[derive(Debug)]
pub struct LocalMemoryClerks {
    connection: String,
    read: Box<CapturedOperationalRead>,
    report: Box<MemoryClerkReport>,
    collected_from: String,
    collected_until: String,
    bytes_read: u64,
}
impl LocalMemoryClerks {
    /// The exact saved journal association verified before this value was constructed.
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &CapturedOperationalRead {
        &self.read
    }
    pub fn into_read(self) -> Box<CapturedOperationalRead> {
        self.read
    }
    pub fn report(&self) -> &MemoryClerkReport {
        &self.report
    }
    /// Client acquisition bounds, kept separately from original native clocks in the raw carrier.
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

/// One original bounded component capture; no current route or native source is acquired by this value.
#[derive(Debug)]
pub struct LocalHyperscaleLogComponents {
    connection: String,
    read: Box<CapturedOperationalRead>,
    report: Box<culprit_core::HyperscaleLogComponentReport>,
    collected_from: String,
    collected_until: String,
    bytes_read: u64,
}
impl LocalHyperscaleLogComponents {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &CapturedOperationalRead {
        &self.read
    }
    pub fn into_read(self) -> Box<CapturedOperationalRead> {
        self.read
    }
    pub fn report(&self) -> &culprit_core::HyperscaleLogComponentReport {
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
