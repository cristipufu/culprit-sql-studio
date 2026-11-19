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
    #[serde(deserialize_with = "Option::deserialize")]
    pub comparison: Option<Box<CacheEntryPopulationComparison>>,
}

/// Two authenticated selected sets, never a database-wide cache census or a creation/eviction history.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CacheEntryPopulationComparison {
    pub title: String,
    pub summary: String,
    pub entries_before: u32,
    pub entries_after: u32,
    pub bytes_before: String,
    pub bytes_after: String,
    pub limitations: Vec<String>,
    pub retained_entries: Vec<CacheEntryRetainedChange>,
    pub additional_contexts: Vec<CacheEntryContextDifference>,
}

/// Native counters belong to this exact retained entry incarnation, not to all copies of its statements.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CacheEntryRetainedChange {
    pub baseline_observation_key: String,
    pub observation_key: String,
    pub metrics: Vec<Metric>,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
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
