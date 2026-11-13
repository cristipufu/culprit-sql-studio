//! Offline query analysis retains its authenticated native input; it is not a newly collected database snapshot.
use crate::{
    AnalysisProvenance, CheckAssessment, Finding, LockedCheck, Workload,
    telemetry::{
        CaptureIdentity, CollectionDescription, ExportedRecord, NativeIdentity, StatementReferences, TelemetryStream,
    },
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

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryInput {
    pub query_id: String,
    pub references: StatementReferences,
    pub native_rows: Vec<NativeIdentity>,
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
    WaitWithoutRuntime,
    ExecutableFixPrerequisitesUnknown,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedAnalysisLimitation {
    pub record_id: Option<String>,
    pub reason: CollectedAnalysisLimit,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedAnalysisContext {
    pub range_id: String,
    pub manifest_sha256: String,
    pub source: CollectionDescription,
    pub capture: CaptureIdentity,
    pub collected_from: String,
    pub collected_until: String,
    pub selected_streams: Vec<TelemetryStream>,
    pub read_gaps: Vec<EvidenceGap>,
    pub records: Vec<CollectedAnalysisRecord>,
    pub query_inputs: Vec<CollectedQueryInput>,
}

/// A partial analysis of the captured application's query observations. It deliberately has no current-server
/// metadata or database-wide health summary. Native outcomes, exact counters and unmapped metrics remain in context.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedQueryAnalysis {
    pub context: CollectedAnalysisContext,
    pub workload: Option<Workload>,
    pub findings: Vec<Finding>,
    pub assessments: Vec<CheckAssessment>,
    pub locked: Vec<LockedCheck>,
    pub limitations: Vec<CollectedAnalysisLimitation>,
    pub provenance: Option<AnalysisProvenance>,
}
