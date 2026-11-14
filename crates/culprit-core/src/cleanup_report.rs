//! Mechanisms describe native cleanup evidence without health grades, allocation ownership or inferred work rates.
use crate::{
    Metric, SourceOutcome,
    telemetry::{CleanupHorizon, NativeIdentity},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CleanupReference {
    pub source: String,
    pub observation_key: String,
    /// Retainer rows have no stored key. A UI joins their complete native identity without reimplementing hashing.
    pub identity: Option<NativeIdentity>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CleanupMechanism {
    SessionHorizon,
    ReplicationSlotHorizon,
    PreparedTransactionHorizon,
    PersistentVersionStore,
    SecondaryLowWatermark,
    OldestActiveTransaction,
    LongSnapshotScan,
    OnlineIndexRebuild,
    OldestAbortedTransaction,
    ConfirmedDatabaseTransaction,
    UnattributedVersioning,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CleanupBoundary {
    pub horizon: CleanupHorizon,
    /// Native visibility, metadata and comparable distances are all required. Never historical completeness.
    pub complete: bool,
    /// All ties among the observed comparable distances, not an exclusive owner or an unqualified global oldest.
    pub oldest_observed: Vec<CleanupReference>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CleanupExplanation {
    pub reference: CleanupReference,
    pub mechanisms: Vec<CleanupMechanism>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CleanupReport {
    pub sources: Vec<SourceOutcome>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub boundaries: Vec<CleanupBoundary>,
    pub entries: Vec<CleanupExplanation>,
}
