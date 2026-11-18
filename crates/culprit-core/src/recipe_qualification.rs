//! Operator-visible qualification state contains no private native scope or approval authority.
use crate::telemetry::{CaptureIdentity, ReadWorkSummary};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipeQualificationState {
    Installed,
    Revoked,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RecipeQualificationGap {
    NativeMeasurementsUnavailable,
    PartialNativeMeasurementWindow,
    NativeGrantEvidenceUnavailable,
    ActiveCancellationUnproved,
    UnsupportedNativeProfile,
    NativePolicyRejected,
    SourceNotComplete,
    ConnectionChanged,
    ApprovalRetired,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct RecipeQualificationSummary {
    pub recipe_id: String,
    pub recipe_version: u32,
    pub state: RecipeQualificationState,
    pub approval_id: Option<String>,
    pub capture: Option<CaptureIdentity>,
    pub source_queries: Option<u32>,
    pub read_work: Option<ReadWorkSummary>,
    pub gaps: Vec<RecipeQualificationGap>,
}

/// Only executable provider policy issues this decision; serialized evidence cannot instantiate authority.
pub enum RecipeQualificationAssessment {
    Approve(Box<crate::telemetry::RecipeCostReceipt>),
    Refuse(Vec<RecipeQualificationGap>),
}
