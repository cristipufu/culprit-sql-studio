//! A census is an observed native population, never a diagnosis or a transport-connection count.
use crate::{MeasurementScope, SourceOutcome, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SessionCensusField {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct SessionCensusEntry {
    pub activity: ActivityKey,
    pub reference: Option<DiagnosticEvidenceRef>,
    pub scope: ActivityScopeBasis,
    pub idle: bool,
    pub title: String,
    pub state: String,
    pub fields: Vec<SessionCensusField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct SessionCensusReport {
    pub context: NativeReadContext,
    pub observed_at: String,
    pub scope: MeasurementScope,
    pub population: ActivityPopulation,
    pub native_source: StateSourceCoverage,
    pub sources: Vec<SourceOutcome>,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub observed_sessions: u32,
    pub observed_requests: u32,
    pub observed_active_sessions: u32,
    pub observed_blocked_sessions: u32,
    pub presented_rows: u32,
    pub read_complete: bool,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<SessionCensusEntry>,
    pub client_groups: SessionCensusClientGroups,
}

/// Exact labels from one native session population. Equality is grouping, never authenticated application ownership.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[ts(export)]
pub struct SessionClientGroupKey {
    pub database_id: String,
    pub database_name: Option<String>,
    pub host: Option<String>,
    pub application: Option<String>,
    pub library: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SessionCensusClientGroup {
    pub key: SessionClientGroupKey,
    pub observed_sessions: u32,
    pub observed_requests: u32,
    pub limitations: Vec<String>,
    pub fields: Vec<SessionCensusField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct SessionCensusClientGroups {
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub grouped_sessions: u32,
    pub ungrouped_sessions: u32,
    pub available_groups: u32,
    pub entries_limited: bool,
    pub entries: Vec<SessionCensusClientGroup>,
}
