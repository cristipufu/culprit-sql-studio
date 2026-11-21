//! Shared native point evidence is informational. Neither a queue nor a memory size identifies a database cause.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct InstanceResourceField {
    pub key: String,
    /// Labels include units. Native integers retain exact digits; derived intervals retain their formula inputs.
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
    /// Two independently visible native node populations, never merged with scheduler or aggregate clocks.
    pub numa_nodes: Option<Box<NumaNodeReport>>,
    /// A separate processor-source point, never merged into the resource point's clock or populations.
    pub process_cpu: Option<Box<ProcessCpuReport>>,
    /// Independently authenticated original CPU captures, never current database workload ownership.
    pub process_cpu_originals: Option<Vec<ProcessCpuReport>>,
    pub process_cpu_interval: Option<Box<ProcessCpuIntervalReport>>,
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
    pub capture: Option<DiagnosticCaptureRef>,
    /// Independent native populations; ordinary legacy totals have no newly acquired populations.
    pub populations: Vec<ProcessCpuPopulationReport>,
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

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NumaNodeFieldAvailability {
    Observed,
    NativeNull,
    ColumnUnavailable,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NumaNodeField {
    pub native_name: String,
    pub label: String,
    pub value: Option<String>,
    pub availability: NumaNodeFieldAvailability,
    pub unit: Option<CounterUnit>,
    pub nature: Option<CounterNature>,
    pub reset: Option<crate::ResetMarker>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NumaNodeEntry {
    pub observation_key: String,
    pub node_id: String,
    pub title: String,
    pub fields: Vec<NumaNodeField>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NumaNodePopulationReport {
    pub kind: MssqlNumaNodeKind,
    pub native_source: StateSourceCoverage,
    pub source_state: SourceState,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub entries: Vec<NumaNodeEntry>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NumaNodeReport {
    pub scope: crate::MeasurementScope,
    pub context: Option<ObservationReadContext>,
    pub read_complete: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub populations: Vec<NumaNodePopulationReport>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ProcessCpuPopulationReport {
    pub native_source: StateSourceCoverage,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub observed_at: Option<String>,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<InstanceResourceGroup>,
}

/// Only the pure original-pair interpreter derives this interval; it establishes no exclusive CPU cause.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ProcessCpuIntervalReport {
    pub available: bool,
    pub first: Option<DiagnosticCaptureRef>,
    pub second: Option<DiagnosticCaptureRef>,
    pub first_context: Option<Box<NativeReadContext>>,
    pub second_context: Option<Box<NativeReadContext>>,
    pub title: String,
    pub summary: String,
    /// Approximate derived percentage plus exact numerator, native elapsed milliseconds and logical-CPU denominator.
    pub fields: Vec<InstanceResourceField>,
    pub limitations: Vec<String>,
}
