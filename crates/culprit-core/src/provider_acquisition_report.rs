//! Current normal acquisition metadata is distinct from captured historical sources and native authority.
use crate::{
    Error, ProviderAcquisitionSummary, ServerInfo, SourceConnectionExplanation, SourceNativeIntervalExplanation,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedProviderAcquisition {
    Available { summary: Box<ProviderAcquisitionSummary> },
    Unavailable { reason: crate::i18n::Message },
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ReadWorkExplanation {
    pub summary: String,
    pub template_order: Vec<u32>,
    pub connection_work: Option<SourceConnectionExplanation>,
    pub native_interval: Option<SourceNativeIntervalExplanation>,
    pub limitations: Vec<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ProviderReaderExplanation {
    pub reader_id: u32,
    pub work: ReadWorkExplanation,
    pub setup_work: Option<ReadWorkExplanation>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ProviderAcquisitionReport {
    Available {
        evidence: Box<ProviderAcquisitionSummary>,
        title: String,
        summary: String,
        settlement: String,
        coverage: String,
        total_admissions: Option<String>,
        readers: Vec<ProviderReaderExplanation>,
        limitations: Vec<String>,
    },
    Unavailable {
        reason: String,
    },
}
#[derive(Serialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum OriginalProbeResult {
    Connected {
        server: ServerInfo,
    },
    Failed {
        error: Error,
    },
    /// No original output was materialized after failed finalization; this establishes no original failure.
    NotMaterialized,
}
#[derive(Serialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum ProbeOutcome {
    Connected { server: ServerInfo },
    Failed { error: Error },
    NativeWorkUnconfirmed { original: OriginalProbeResult, error: Error },
    FinalizationFailed { original: OriginalProbeResult, error: Error },
}
#[derive(Serialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ObservedProbeAttempt {
    pub outcome: ProbeOutcome,
    pub acquisition: ProviderAcquisitionReport,
}

#[derive(Serialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum ObservedScanAttempt {
    Complete { report: Box<crate::Report> },
    Failed { error: Error, acquisition: ProviderAcquisitionReport },
    NativeWorkUnconfirmed { original_failure: Option<Error>, error: Error, acquisition: ProviderAcquisitionReport },
    FinalizationFailed { original_failure: Option<Error>, error: Error, acquisition: ProviderAcquisitionReport },
}

impl ObservedProbeAttempt {
    pub fn require_connected(&self) -> crate::Result<&ServerInfo> {
        match &self.outcome {
            ProbeOutcome::Connected { server } => Ok(server),
            ProbeOutcome::Failed { error } | ProbeOutcome::FinalizationFailed { error, .. } => Err(error.clone()),
            ProbeOutcome::NativeWorkUnconfirmed { error, .. } => Err(error.clone()),
        }
    }
}
