//! Exact native counter inputs remain informational and independently scoped, without inferred rates or owners.
use crate::{MeasurementScope, Metric, ResetMarker, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PerformanceCounterAvailability {
    Observed,
    UnknownNativeType,
    RejectedNativeValue,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PerformanceCounterEntry {
    pub observation_key: String,
    pub counter_key: String,
    pub field: String,
    pub family: PerformanceCounterFamily,
    pub family_label: String,
    pub availability: PerformanceCounterAvailability,
    pub native_object: String,
    pub native_name: String,
    pub native_instance: Option<String>,
    pub native_type: Option<String>,
    pub observed_at: Option<String>,
    pub scope: MeasurementScope,
    pub nature: CounterNature,
    pub unit: CounterUnit,
    pub reset: ResetMarker,
    /// Exact signed native text is preserved even when it cannot authorize a measurement.
    pub raw_value: String,
    pub metric: Metric,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PerformanceCounterReport {
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub read_complete: Option<bool>,
    /// Source observations are aggregate sets, independently from the native DMV counter rows inside them.
    pub native_observations: Option<u32>,
    pub observed_observations: u32,
    pub native_counter_rows: Option<u32>,
    pub observed_counter_rows: u32,
    pub presented_counter_rows: u32,
    pub entries_limited: bool,
    pub fields_withheld: bool,
    /// Requested fields/instances without a retained value, never invented native rows or measured zeroes.
    pub missing_fields: Vec<Metric>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<PerformanceCounterEntry>,
    /// The existing shared fraction policy interprets only source-declared same-read pairs.
    pub ratios: Option<Box<crate::CounterRatioReport>>,
}
