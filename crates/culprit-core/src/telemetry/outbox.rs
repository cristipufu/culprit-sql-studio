//! Local capture ordering is meaningful only within the same intact outbox namespace.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

const MIN_OUTBOX_BYTES: u64 = 1024 * 1024;
const MAX_OUTBOX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_OUTBOX_CAPTURES: u32 = 10_000;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CaptureRevision {
    pub outbox_id: String,
    pub owner_epoch: String,
    pub sequence: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CaptureIdentity {
    pub observation_id: String,
    pub revision: Option<CaptureRevision>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct OutboxLimits {
    /// Payloads, manifests and reservations. The journal has a separate fixed size ceiling.
    #[ts(type = "number")]
    pub max_bytes: u64,
    pub max_captures: u32,
}
impl OutboxLimits {
    pub fn is_valid(&self) -> bool {
        (MIN_OUTBOX_BYTES..=MAX_OUTBOX_BYTES).contains(&self.max_bytes)
            && (1..=MAX_OUTBOX_CAPTURES).contains(&self.max_captures)
    }
}
impl Default for OutboxLimits {
    fn default() -> Self {
        Self { max_bytes: 1024 * 1024 * 1024, max_captures: 256 }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum OutboxCaptureState {
    Capturing,
    Ready,
    /// A verified remote commit authorizes removal; recovery finishes an interrupted local deletion.
    Evicting,
    Incomplete,
    Interrupted,
    Quarantined,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceWorkState {
    NotStarted,
    Active,
    Quiescent,
    Unconfirmed,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum OutboxProblem {
    Locked,
    Capacity,
    RecoveryRequired,
    SourceUnconfirmed,
    Integrity,
    UnsupportedFormat,
    MissingCapture,
    Storage,
    InvalidLimits,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct OutboxEntry {
    pub capture: CaptureIdentity,
    pub connection: String,
    pub request: super::CollectionRequest,
    pub state: OutboxCaptureState,
    pub source_work: SourceWorkState,
    #[ts(type = "number")]
    pub charged_bytes: u64,
    pub problem: Option<OutboxProblem>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct OutboxStatus {
    pub outbox_id: String,
    pub limits: OutboxLimits,
    pub captures: u32,
    pub ready: u32,
    pub source_debts: u32,
    /// Unconfirmed native shutdowns, distinct from work currently owned by a running capture.
    pub unconfirmed_sources: u32,
    pub unfinished: u32,
    #[ts(type = "number")]
    pub charged_bytes: u64,
    pub entries: Vec<OutboxEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default)]
#[ts(export)]
pub struct OutboxRecovery {
    pub committed: u32,
    pub interrupted: u32,
    pub quarantined: u32,
    pub source_debts: u32,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CapturedRange {
    pub scope_id: String,
    pub range: super::TelemetryRange,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct OutboxReplay {
    pub capture: CaptureIdentity,
    pub batches: u32,
    #[ts(type = "number")]
    pub bytes: u64,
    pub directory_synced: bool,
}
