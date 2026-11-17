//! A statement link preserves the independently captured definition, namespace and payload availability.
use crate::telemetry::{DiagnosticEvidenceRef, PayloadUnavailable, StatementOrigin};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryStoreDefinitionRead {
    pub namespace: QueryStoreDefinitionNamespace,
    /// Selected authenticated manifest rows, never the entire Query Store query population.
    pub selected_rows: u32,
    pub observed_rows: u32,
    pub read_complete: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum QueryStoreDefinitionNamespace {
    Matched,
    Unverified,
    Changed,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum QueryStoreStatementBinding {
    Captured,
    Matched,
    NotObserved,
    SourceUnverified,
    SourceChanged,
    /// The original runtime reference did not retain a complete native query incarnation.
    IdentityUnavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryStoreStatementExplanation {
    pub binding: QueryStoreStatementBinding,
    pub definition: Option<DiagnosticEvidenceRef>,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub preview: Option<String>,
    /// Presentation truncation of a complete verified body. A warehouse prefix never supplies this authority.
    pub preview_truncated: bool,
    pub unavailable: Option<PayloadUnavailable>,
    /// Rust-rendered availability; surfaces retain the typed reason without duplicating its vocabulary.
    pub unavailable_explanation: Option<String>,
    pub explanation: String,
}
