//! Explanations reference exact captured populations; native bucket rows remain in the evidence once.
use crate::{Metric, ResetMarker, SourceOutcome, telemetry::NativeIdentity};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct RuntimeDistributionReport {
    pub sources: Vec<SourceOutcome>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<RuntimeDistributionExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct RuntimeDistributionExplanation {
    pub observation_key: String,
    pub identity: NativeIdentity,
    pub observed_at: String,
    pub reset: ResetMarker,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}
