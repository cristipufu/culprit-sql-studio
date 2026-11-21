//! Informational current native pool configuration, independent of demand, historical samples and member SLOs.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PoolSettingField {
    pub key: String,
    pub label: String,
    pub native_value: Option<String>,
    pub unit: String,
    pub value: Option<String>,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PoolSettingsReport {
    pub target: Option<CollectionTarget>,
    pub elastic_pool: Option<SharedResourceIdentity>,
    pub elastic_pool_error: Option<crate::ErrorCode>,
    pub member_database: Option<String>,
    pub logical_server: Option<String>,
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: SourceState,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub read_complete: bool,
    pub observed_at: Option<String>,
    pub title: String,
    pub summary: String,
    pub fields: Vec<PoolSettingField>,
    pub limitations: Vec<String>,
    pub investigation: String,
    pub verify: String,
}
