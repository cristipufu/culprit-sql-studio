//! Page-target explanations reference observed waiters and metadata. They never assert transient latch ownership.
use crate::{ObservedWaitResource, SourceState, telemetry::ActivityKey};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<PageWaitExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitExplanation {
    pub observation_key: String,
    /// Small exact references also locate the bare native row in a scan without implementing proof hashing in UI.
    pub request_key: ActivityKey,
    pub resource: ObservedWaitResource,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
