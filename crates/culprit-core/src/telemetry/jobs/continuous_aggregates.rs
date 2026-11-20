//! Current public-view configuration is not refresh coverage or a historical job/object binding.
use super::*;

pub const MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS: u32 = 64;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleRelationLabel {
    pub schema: String,
    pub name: String,
}
impl TimescaleRelationLabel {
    pub fn is_valid(&self) -> bool {
        label(&self.schema, MAX_TIMESCALE_JOB_LABEL_BYTES) && label(&self.name, MAX_TIMESCALE_JOB_LABEL_BYTES)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleContinuousAggregate {
    pub position: u32,
    pub observed_at: String,
    /// Qualified relation names are labels at this point, not globally unique object incarnations.
    pub view: TimescaleRelationLabel,
    pub source_hypertable: TimescaleRelationLabel,
    pub materialization_hypertable: TimescaleRelationLabel,
    pub owner: String,
    pub materialized_only: bool,
    pub compression_enabled: bool,
    pub granular_refresh_enabled: Option<bool>,
}
impl TimescaleContinuousAggregate {
    pub fn is_valid(&self) -> bool {
        self.position > 0
            && self.position <= MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS
            && canonical_time(&self.observed_at).is_ok()
            && self.view.is_valid()
            && self.source_hypertable.is_valid()
            && self.materialization_hypertable.is_valid()
            && label(&self.owner, MAX_TIMESCALE_JOB_LABEL_BYTES)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE, self.position, &self.view))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
}
