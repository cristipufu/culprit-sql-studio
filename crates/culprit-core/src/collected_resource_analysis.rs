//! Explanations over an authenticated resource capture retain the native rows once, without creating a database.
use crate::{RetainedFailureReport, collected_analysis::CollectedEvidenceContext};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedRetainedFailureAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: RetainedFailureReport,
}
