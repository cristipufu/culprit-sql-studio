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
