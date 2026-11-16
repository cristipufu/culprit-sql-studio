//! Replication explanations reference original native points; neither queue sizes nor timings establish a SQL owner.
use crate::{MeasurementScope, SourceOutcome, t, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ReplicationReference {
    pub source: String,
    pub observation_key: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ReplicationCondition {
    MovementSuspended,
    NotSynchronizing,
    Disconnected,
    ReceiverStopped,
    ApplyingStopped,
    ThreadsStopped,
    Connecting,
    SlotInvalid,
    WalUnreserved,
    ReplayPaused,
}

impl ReplicationCondition {
    pub fn label(self) -> String {
        match self {
            Self::MovementSuspended => t!("view.replication.condition.movement_suspended"),
            Self::NotSynchronizing => t!("view.replication.condition.not_synchronizing"),
            Self::Disconnected => t!("view.replication.condition.disconnected"),
            Self::ReceiverStopped => t!("view.replication.condition.receiver_stopped"),
            Self::ApplyingStopped => t!("view.replication.condition.applying_stopped"),
            Self::ThreadsStopped => t!("view.replication.condition.threads_stopped"),
            Self::Connecting => t!("view.replication.condition.connecting"),
            Self::SlotInvalid => t!("view.replication.condition.slot_invalid"),
            Self::WalUnreserved => t!("view.replication.condition.wal_unreserved"),
            Self::ReplayPaused => t!("view.replication.condition.replay_paused"),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ReplicationField {
    pub key: String,
    pub label: String,
    /// Already formatted for the reader; the original exact native value remains in its referenced point.
    pub value: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ReplicationFieldGroup {
    pub title: String,
    pub fields: Vec<ReplicationField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ReplicationEntry {
    pub reference: ReplicationReference,
    pub scope: MeasurementScope,
    pub conditions: Vec<ReplicationCondition>,
    pub title: String,
    pub explanation: String,
    pub fields: Vec<ReplicationField>,
    pub groups: Vec<ReplicationFieldGroup>,
    pub session: Option<NativeActivityIdentity>,
    pub unresolved_session_id: Option<String>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ReplicationReport {
    pub context: ReplicationReadContext,
    pub sources: Vec<SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub state_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<ReplicationEntry>,
}
