//! Informational catalog properties never establish a problem, replica owner or permission to change settings.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct DatabasePropertyField {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
    pub native_code: Option<String>,
    pub native_description: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct DatabasePropertiesReport {
    pub target: Option<CollectionTarget>,
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: Option<SourceState>,
    pub read_complete: Option<bool>,
    pub observation_key: Option<String>,
    pub observed_at: Option<String>,
    pub native_database_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub fields: Vec<DatabasePropertyField>,
}
