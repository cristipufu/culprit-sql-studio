//! Derived plan analysis refers to authenticated definitions; original SQL and XML remain in the evidence context.
use crate::{Plan, SourceOutcome, collected_analysis::CollectedEvidenceContext, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CollectedCachedPlanAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: CachedPlanReport,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CachedPlanReport {
    pub sources: Vec<SourceOutcome>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CachedPlanEntry>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CachedPlanEntry {
    pub source: String,
    pub definition_key: String,
    pub definition_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub identity: Option<CachedPlanIdentity>,
    /// The derived operator tree and its existing insights, without another copy of SQL or raw XML.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<Plan>,
    pub limitations: Vec<CachedPlanLimitation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CachedPlanLimitation {
    pub reason: CachedPlanLimit,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum CachedPlanIdentity {
    Mssql(CachedStatementIdentity),
    Native(NativePlanIdentity),
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CachedPlanPayload {
    Statement,
    EstimatedPlan,
    RetainedPlan,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CachedPlanLimit {
    UnsupportedDefinition,
    NativeParserUnavailable,
    PayloadUnavailable { payload: CachedPlanPayload, reason: PayloadUnavailable },
    UnusablePlan,
    MissingActualMeasurements,
    UnknownRelationalInput,
    MissingCurrentPrerequisites,
}
