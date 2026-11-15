//! Cached grant explanations retain exact read and entry references, independently of current request ownership.
use crate::{
    Metric, SourceOutcome,
    i18n::Message,
    telemetry::{CachedStatementIdentity, StateSourceCoverage},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementGrantReport {
    pub sources: Vec<SourceOutcome>,
    pub reads: Vec<StatementGrantRead>,
    pub title: Message,
    pub summary: Message,
    pub limitations: Vec<Message>,
    pub entries: Vec<StatementGrantExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementGrantRead {
    /// Identifies the original read without pooling overlapping Largest/Hashes selections.
    pub snapshot_key: String,
    pub source_scope_id: String,
    pub coverage: Vec<StateSourceCoverage>,
    /// Whether every selected original row was decoded. Native source coverage remains independent.
    pub read_complete: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementGrantExplanation {
    /// Together these identify one original row, including its native read bounds and source owner.
    pub snapshot_key: String,
    pub observation_key: String,
    pub identity: CachedStatementIdentity,
    pub title: Message,
    pub explanation: Message,
    pub metrics: Vec<Metric>,
    pub investigation: Message,
    pub verify: Message,
}
