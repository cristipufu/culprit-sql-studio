//! Native operation progress remains separate from incident causality, completion and resource ownership.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MaintenanceProgressEntry {
    pub observation_key: String,
    pub actor: ActivityKey,
    pub database_oid: String,
    pub relation: Option<NativeIdentity>,
    pub index: Option<NativeIdentity>,
    pub command: MaintenanceCommand,
    pub phase: String,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MaintenanceProgressReport {
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
    pub entries: Vec<MaintenanceProgressEntry>,
}
