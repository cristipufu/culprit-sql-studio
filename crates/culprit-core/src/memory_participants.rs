//! Native reservations and tracked session allocations have different meanings. Their explanations reference
//! captured facts without copying native data or declaring a current statement to be the allocating statement.
use crate::SourceState;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MemoryParticipantsReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<MemoryParticipantExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MemoryParticipantExplanation {
    /// The native observation's key in this capture, not an incident or causal-owner identity.
    pub observation_key: String,
    /// Exact native reference also permits a scan UI to find its bare native row without hashing identifiers.
    pub observation_identity: crate::telemetry::NativeIdentity,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
