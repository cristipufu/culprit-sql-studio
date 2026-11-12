//! Source semantics exposed to consumers. Every engine explicitly supplies these facts; there is no fallback engine.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StatisticsFreshness {
    PerObject,
    PerTable,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct EvidencePresentation {
    pub storage_about: String,
    pub storage_untimed: String,
    pub health_empty: String,
    pub generic_plan_context: String,
    pub statistics_freshness: StatisticsFreshness,
    pub statistics_absence_known: bool,
    pub workload_windowed_by_default: bool,
    pub deadlock_time_window: bool,
    pub retained_actual_plans: bool,
}
