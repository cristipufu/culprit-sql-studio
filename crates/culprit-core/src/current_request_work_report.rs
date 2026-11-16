//! One request's native counters remain observations, separate from delay, resource limits and incident causality.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CurrentRequestWorkEntry {
    pub observation_key: String,
    pub observed_at: String,
    pub actor: ActivityKey,
    pub database: String,
    pub scope: ActivityScopeBasis,
    pub statement_identity: NativeIdentity,
    pub statement: DefinitionPayload,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CurrentRequestWorkReport {
    pub source: String,
    pub context: NativeReadContext,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub binding: FocusedRequestBinding,
    pub membership: FocusedRequestMembership,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CurrentRequestWorkEntry>,
}
