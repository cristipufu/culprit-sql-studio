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
/// Some sources reset extrema independently from their cumulative mean and variance.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct StatementDistribution {
    pub statistics: RuntimeMetric,
    pub reset: crate::ResetMarker,
    pub extrema_reset: crate::ResetMarker,
}
impl StatementDistribution {
    fn is_valid(&self) -> bool {
        self.statistics.is_valid() && valid_reset(&self.reset) && valid_reset(&self.extrema_reset)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementCounterSample {
    pub identity: NativeQueryIdentity,
    /// A database observed on one distributed node is not a database-wide or cluster-wide total.
    pub scope: crate::MeasurementScope,
    pub observed_at: String,
    /// Unknown remains unknown: two observations alone do not establish a safe delta.
    pub reset: crate::ResetMarker,
    pub totals: Vec<StatementCounter>,
    pub distributions: Vec<StatementDistribution>,
}
impl StatementCounterSample {
    pub fn key(&self) -> crate::Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        let mut distributions = std::collections::BTreeSet::new();
        self.identity.is_valid()
            && valid_reset(&self.reset)
            && self.observed_at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.observed_at).is_ok()
            && !self.totals.is_empty()
            && self.totals.len() <= MAX_STATEMENT_METRICS
            && self.totals.iter().all(|metric| metric.is_valid() && keys.insert(&metric.key))
            && self.distributions.len() <= MAX_STATEMENT_METRICS
            && self
                .distributions
                .iter()
                .all(|distribution| distribution.is_valid() && distributions.insert(&distribution.statistics.key))
    }
}
