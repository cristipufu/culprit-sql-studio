//! Explanations of retained native failures. These observations do not establish a current database
//! opportunity, an exact incident count or a query that owns the exhausted resource.

use crate::{
    SourceState,
    telemetry::{CollectionDescription, DiagnosticInputQualification, diagnostic_original_input_qualification},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RetainedFailureReport {
    pub incident_input: DiagnosticInputQualification,
    pub incident_input_explanation: String,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    /// One explanation per native row. Equal native observations retain distinct capture-local positions.
    pub entries: Vec<RetainedFailureExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RetainedFailureExplanation {
    /// References one original native row in the input snapshot, without cloning its potentially large JSON.
    pub position: u32,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

impl RetainedFailureReport {
    /// The authenticated original header qualifies incident grouping only; raw failure evidence remains independent.
    pub fn qualify_incident_input(&mut self, source: &CollectionDescription) {
        self.incident_input = diagnostic_original_input_qualification(Some(source));
        self.incident_input_explanation = self.incident_input.explanation();
    }
}
