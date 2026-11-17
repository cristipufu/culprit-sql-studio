//! Database sessions and instance connection attempts remain distinct native populations in every consumer.
use crate::{
    ConnectionOutcomeAvailability, ConnectionOutcomeMetricKind, MeasurementScope, ResetMarker, SourceOutcome,
    telemetry::*,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ConnectionOutcomeReference {
    pub source: String,
    pub observation_key: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConnectionOutcomeMetric {
    pub kind: ConnectionOutcomeMetricKind,
    pub native_key: String,
    pub label: String,
    pub availability: ConnectionOutcomeAvailability,
    /// Exact native integer text or the original native floating-point value; missing is not measured zero.
    pub value: Option<MeasurementValue>,
    pub display_value: Option<String>,
    pub unit: Option<CounterUnit>,
    pub nature: Option<CounterNature>,
    pub reset: ResetMarker,
    pub rate: Option<crate::ConnectionOutcomeRate>,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConnectionOutcomeEntry {
    pub reference: ConnectionOutcomeReference,
    pub scope: MeasurementScope,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<ConnectionOutcomeMetric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConnectionOutcomeReport {
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub sources: Vec<SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub mapped_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<ConnectionOutcomeEntry>,
}
