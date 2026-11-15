//! Explanations reference original whole-entry observations without charging their bytes to each statement.
use crate::{Metric, SourceState};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CacheEntryMemoryReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CacheEntryMemoryExplanation>,
    pub context_differences: Vec<CacheEntryContextDifference>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CacheEntryMemoryExplanation {
    pub observation_key: String,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CacheEntryContextDifference {
    pub left_observation_key: String,
    pub right_observation_key: String,
    pub attributes: Vec<String>,
    pub explanation: String,
    pub investigation: String,
}
