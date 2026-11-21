//! Shared native point evidence is informational. Neither a queue nor a memory size identifies a database cause.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct InstanceResourceField {
    pub key: String,
    /// Labels include native units; numbers stay exact unformatted digits.
    pub label: String,
    pub value: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct InstanceResourceGroup {
    pub key: String,
    pub title: String,
    pub fields: Vec<InstanceResourceField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct InstanceResourceReport {
    /// A separate processor-source point, never merged into the resource point's clock or populations.
    pub process_cpu: Option<Box<ProcessCpuReport>>,
    /// Per-scheduler rows are an independent source population, never the aggregate resource point.
    pub schedulers: Option<Box<SchedulerReport>>,
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: SourceState,
    pub read_complete: Option<bool>,
    pub observed_at: Option<String>,
    /// This describes the bounded source populations, independently of body or optional-read admission.
    pub native_populations_complete: Option<bool>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub groups: Vec<InstanceResourceGroup>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ProcessCpuReport {
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: SourceState,
    pub read_complete: Option<bool>,
    pub observed_at: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub fields: Vec<InstanceResourceField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SchedulerFieldAvailability {
    Observed,
    NativeNull,
    ColumnUnavailable,
    InstrumentationUnqualified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SchedulerField {
    pub native_name: String,
    pub label: String,
    /// Only an admitted native property or measurement. An unqualified diagnostic value is kept separately.
    pub value: Option<String>,
    pub unqualified: Option<SchedulerUnqualifiedValue>,
    pub availability: SchedulerFieldAvailability,
    pub explanation: Option<String>,
    /// Properties have no invented numeric unit or counter semantics.
    pub unit: Option<CounterUnit>,
    pub nature: Option<CounterNature>,
    pub reset: Option<crate::ResetMarker>,
}

/// Column presence does not establish that optional native instrumentation was enabled. This is raw evidence.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SchedulerUnqualifiedValue {
    pub native_value: String,
    pub required_global_trace_flag: u32,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SchedulerEntry {
    pub observation_key: String,
    /// Native local scheduler ID; neither a request owner nor a cross-capture generation.
    pub scheduler_id: String,
    pub title: String,
    pub fields: Vec<SchedulerField>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SchedulerReport {
    pub scope: crate::MeasurementScope,
    pub population: String,
    /// No per-row timestamp is invented from the acquisition interval or another DMV point.
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub read_complete: Option<bool>,
    /// Original source records; partial/unavailable coverage does not establish the visible scheduler total.
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<SchedulerEntry>,
}
