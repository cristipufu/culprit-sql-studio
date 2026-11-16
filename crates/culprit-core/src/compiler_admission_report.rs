//! Explanations reference raw compiler facts once. A waiting request is affected, never a named memory holder.
use crate::{
    SourceState,
    telemetry::{ActivityKey, NativeIdentity},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilerAdmissionReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub waiters: Vec<CompilerWaiterExplanation>,
    pub gateways: Vec<CompilerGatewayExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilerWaiterExplanation {
    pub observation_key: String,
    pub activity: ActivityKey,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilerGatewayExplanation {
    pub observation_key: String,
    pub identity: NativeIdentity,
    pub title: String,
    pub explanation: String,
}
