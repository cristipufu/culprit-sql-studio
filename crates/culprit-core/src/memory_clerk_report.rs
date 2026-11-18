//! Native allocation dimensions remain separate exact gauges, without pressure or statement ownership.
use crate::{MeasurementScope, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MemoryClerkField {
    pub key: String,
    /// The localized label includes the native unit; value retains exact unformatted digits.
    pub label: String,
    pub value: Option<String>,
    pub unit: CounterUnit,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MemoryClerkEntry {
    pub observation_key: String,
    pub scope: MeasurementScope,
    pub clerk_type: String,
    pub name: String,
    pub memory_node_id: String,
    pub explanation: String,
    pub fields: Vec<MemoryClerkField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MemoryClerkReport {
    /// A read interval is not a per-group timestamp or an allocation lifetime.
    pub context: Option<ObservationReadContext>,
    /// Original native coverage is independent from missing retained bodies and presentation limits.
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    /// None when a snapshot retained no original body-completion declaration.
    pub read_complete: Option<bool>,
    /// Native group rows, never the sum of clerk_count or allocation dimensions.
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<MemoryClerkEntry>,
}
