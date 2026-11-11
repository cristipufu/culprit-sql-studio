//! Cumulative statement totals are point observations, never fabricated native intervals.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementCounter {
    pub key: RuntimeMetricKey,
    pub unit: RuntimeMetricUnit,
    pub total: MeasurementValue,
}
impl StatementCounter {
    fn is_valid(&self) -> bool {
        self.key.accepts(self.unit)
            && self.total.is_valid()
            && (!matches!(self.key, RuntimeMetricKey::Executions | RuntimeMetricKey::Rows)
                || matches!(self.total, MeasurementValue::Integer(_)))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementCounterSample {
    pub identity: NativeQueryIdentity,
    pub observed_at: String,
    /// Unknown remains unknown: two observations alone do not establish a safe delta.
    pub reset: crate::ResetMarker,
    pub totals: Vec<StatementCounter>,
}
impl StatementCounterSample {
    pub fn key(&self) -> crate::Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        self.identity.is_valid()
            && valid_reset(&self.reset)
            && self.observed_at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.observed_at).is_ok()
            && !self.totals.is_empty()
            && self.totals.len() <= MAX_STATEMENT_METRICS
            && self.totals.iter().all(|metric| metric.is_valid() && keys.insert(&metric.key))
    }
}
