//! Independent retained tuning facts reference native rows; they do not authorize a workload join or a command.
use crate::{QueryStoreScope, SourceOutcome, i18n::Message};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TuningReport {
    pub sources: Vec<SourceOutcome>,
    pub title: Message,
    pub summary: Message,
    pub limitations: Vec<Message>,
    pub entries: Vec<TuningExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TuningExplanation {
    /// Exact native source and scope-sensitive row key in this capture. Neither is a query-definition reference.
    pub source: String,
    pub observation_key: String,
    /// Plan-reference scope for recommendations/forcing; each hint retains its own scope. None for options.
    pub scope: Option<QueryStoreScope>,
    pub title: Message,
    pub explanation: Message,
    pub investigation: Message,
    pub verify: Message,
}
