//! Offline query analysis retains its authenticated native input; it is not a newly collected database snapshot.
use crate::{
    AnalysisProvenance, CheckAssessment, Finding, LockedCheck, Workload,
    telemetry::{CaptureIdentity, CollectionDescription, ExportedRecord, StatementReferences, TelemetryStream},
};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum EvidenceGapReason {
    NotPublished,
    FamilyNotCaptured,
    ProjectionProofUnavailable,
    MissingRows,
    DetailInRecovery,
    NativeRepresentationRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct EvidenceGap {
    pub stream: Option<TelemetryStream>,
    pub batch_id: Option<String>,
    pub row_ordinal: Option<u32>,
    pub reason: EvidenceGapReason,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedAnalysisRecord {
    pub batch_id: String,
    pub row_ordinal: u32,
    pub native_payload_sha256: String,
    pub row: ExportedRecord,
}

/// Work performed by this archive read, independently of native source coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct CollectedEvidenceMetrics {
    pub selected_rows: u32,
    pub queries: u32,
    pub pages: u32,
    pub returned_rows: u32,
    pub response_bytes: u64,
    pub proof_bytes: u64,
    pub recovery_objects: u32,
    pub recovery_bytes: u64,
}

/// Proof and native input for one selected capture. Domain views reference these records instead of cloning them.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedEvidenceContext {
    pub range_id: String,
    pub recovery_manifest_sha256: String,
    pub capture_manifest_sha256: String,
    pub source: CollectionDescription,
    pub capture: CaptureIdentity,
    pub collected_from: String,
    pub collected_until: String,
    pub selected_streams: Vec<TelemetryStream>,
    pub read_gaps: Vec<EvidenceGap>,
    pub metrics: CollectedEvidenceMetrics,
    pub records: Vec<CollectedAnalysisRecord>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryInput {
    pub query_id: String,
    pub references: StatementReferences,
    pub measurements: Vec<crate::telemetry::NativeMeasurementKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectedAnalysisLimit {
    MissingCurrentServerMetadata,
    MissingSchema,
    MissingSettings,
    MissingResourceMeasurements,
    MissingQueryDefinition,
    MissingPlanDefinition,
    QueryTextUnavailable,
    PlanUnavailable,
    UnknownStatementOrigin,
    CounterPrecision,
    RuntimeBounds,
    RuntimeDenominatorUnavailable,
    DurationUnavailable,
    MetricNotProjected,
    WaitSemanticsUnavailable,
    WaitCaptureDisabled,
    WaitCaptureUnavailable,
    WaitIntervalCoverageUnverified,
    WaitFamilyIncomplete,
    UnknownWaitCategory,
    WaitWithoutRuntime,
    ExecutableFixPrerequisitesUnknown,
    ExecutionPopulationUnknown,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedAnalysisLimitation {
    pub record_id: Option<String>,
    pub reason: CollectedAnalysisLimit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectedQueryPopulation {
    Unknown,
    Unpartitioned,
    Population { identity: crate::telemetry::NativeIdentity },
}

/// A partial analysis of the captured application's query observations. It deliberately has no current-server
/// metadata or database-wide health summary. Native outcomes, exact counters and unmapped metrics remain in context.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryAnalysis {
    pub context: CollectedEvidenceContext,
    pub requested_population: Option<crate::telemetry::NativeIdentity>,
    /// Independent results from the same authenticated capture. Their runtime populations are never pooled.
    pub analyses: Vec<CollectedQueryAnalysisResult>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryAnalysisResult {
    /// Exact counts survive an unavailable time denominator. Totals are confined to this original population.
    pub runtime_totals: Option<CollectedQueryRuntimeTotals>,
    pub population: CollectedQueryPopulation,
    pub query_inputs: Vec<CollectedQueryInput>,
    pub workload: Option<Workload>,
    pub findings: Vec<Finding>,
    pub assessments: Vec<CheckAssessment>,
    pub locked: Vec<LockedCheck>,
    pub limitations: Vec<CollectedAnalysisLimitation>,
    pub provenance: Option<AnalysisProvenance>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectedRuntimeTimeTotal {
    Complete {
        total_ms: f64,
    },
    /// A missing mean, unrepresentable execution multiplier or invalid derived sum cannot become zero cost.
    Incomplete,
}
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryRuntimeTotals {
    pub executions: String,
    pub query_identities: String,
    pub duration: CollectedRuntimeTimeTotal,
    pub cpu: CollectedRuntimeTimeTotal,
}
