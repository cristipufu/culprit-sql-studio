//! Current allocated and used table/index gauges retain their separate native units and scope.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableMemoryReport {
    /// One indivisible original scope/target/clock; sharing withdraws it rather than manufacturing a masked owner.
    pub original: Option<TableMemoryOrigin>,
    pub source: crate::SourceOutcome,
    pub read_complete: bool,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub title: String,
    pub summary: String,
    pub metrics: Vec<crate::Metric>,
    pub limitations: Vec<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TableMemoryOrigin {
    pub context: crate::telemetry::NativeReadContext,
    pub target: crate::telemetry::MssqlObjectIdentity,
    pub metadata: crate::telemetry::MssqlTableMemoryMetadata,
}
