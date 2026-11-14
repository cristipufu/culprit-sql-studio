//! Native access counts remain separate from operator output and complete relational-input cardinality.
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq)]
#[ts(export)]
pub struct PlanAccessRows {
    /// Native estimated rows accessed before the residual predicate, per operator execution.
    #[serde(default, with = "super::nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub estimated_per_execution: Option<f64>,
    /// Native rows accessed across the same recorded executions/workers as the operator's actual output.
    /// All participating native counters must be present. This is neither an estimate fallback nor proof that
    /// the operator consumed the whole parameter, table or function result.
    #[serde(default, with = "super::nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub actual_total: Option<f64>,
}

impl PlanAccessRows {
    pub fn new(estimated_per_execution: Option<f64>, actual_total: Option<f64>) -> Option<Self> {
        let rows = Self {
            estimated_per_execution: estimated_per_execution.and_then(super::nonnegative),
            actual_total: actual_total.and_then(super::nonnegative),
        };
        rows.is_valid().then_some(rows)
    }

    pub fn is_valid(&self) -> bool {
        (self.estimated_per_execution.is_some() || self.actual_total.is_some())
            && [self.estimated_per_execution, self.actual_total]
                .into_iter()
                .flatten()
                .all(|value| value.is_finite() && value >= 0.0)
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<PlanAccessRows>, D::Error> {
    Ok(Option::<PlanAccessRows>::deserialize(deserializer)?.filter(PlanAccessRows::is_valid))
}

#[cfg(test)]
mod tests;
