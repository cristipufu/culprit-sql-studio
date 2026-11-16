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
    /// Each independently verified resource roster is retained once for this read, never once per waiter.
    pub admission_resources: Vec<MemoryGrantAdmission>,
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
    /// Same-read admission context only. It neither establishes a blocker nor complete shared-resource coverage.
    pub grant_admission: Option<crate::telemetry::NativeIdentity>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MemoryGrantAdmission {
    pub resource: crate::telemetry::NativeIdentity,
    pub participants: Vec<MemoryParticipantReference>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MemoryParticipantReference {
    pub observation_key: String,
    pub observation_identity: crate::telemetry::NativeIdentity,
}
