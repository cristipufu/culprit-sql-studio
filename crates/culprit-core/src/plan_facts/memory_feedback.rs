//! The native optimizer's feedback verdict is independent of present spill harm or execution-population ownership.
use crate::{Engine, Plan, PlanKind};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanMemoryGrantFeedbackState {
    NotRecorded,
    FirstExecution,
    AccurateGrant,
    /// Native `No: Feedback disabled`: repeated grant-increase/decrease adjustments disabled the feedback loop.
    DisabledUnstable,
    Adjusting,
    PercentileAdjusting,
    Stable,
    /// A native state was present but could not be interpreted. Its original text remains in the native payload.
    Unrecognized,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanMemoryGrantFeedbackIssue {
    InvalidState,
    InvalidPreviousGrant,
    AmbiguousMetadata,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanMemoryGrantFeedback {
    pub state: PlanMemoryGrantFeedbackState,
    /// Native LastRequestedMemory: granted KB from the prior execution, not the current request or memory use.
    /// A canonical integer string preserves values beyond binary64 precision, including an explicitly recorded zero.
    pub previous_grant_kb: Option<String>,
    /// Valid runtime counters were recorded inside this same selected native QueryPlan. This is not a timestamp,
    /// request identity, history-group binding or a claim that the execution is still running.
    pub runtime_observed: bool,
    pub issues: Vec<PlanMemoryGrantFeedbackIssue>,
}

impl PlanMemoryGrantFeedback {
    pub fn is_valid(&self) -> bool {
        self.issues.len() <= 3
            && self.issues.iter().collect::<BTreeSet<_>>().len() == self.issues.len()
            && self.previous_grant_kb.as_deref().is_none_or(|value| {
                value.len() <= 20 && value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
            })
            && (!self.issues.contains(&PlanMemoryGrantFeedbackIssue::InvalidPreviousGrant)
                || self.previous_grant_kb.is_none())
            && (!self.issues.contains(&PlanMemoryGrantFeedbackIssue::InvalidState)
                || self.state == PlanMemoryGrantFeedbackState::Unrecognized)
            && (!self.issues.contains(&PlanMemoryGrantFeedbackIssue::AmbiguousMetadata)
                || self.state == PlanMemoryGrantFeedbackState::Unrecognized
                    && self.previous_grant_kb.is_none()
                    && !self.runtime_observed)
    }

    /// One implementation of the native-mechanism boundary for insights and finding consumers. A saved display
    /// property, a version, or runtime counters from an unselected statement cannot authorize this interpretation.
    pub fn disabled_in(plan: &Plan) -> Option<&Self> {
        let feedback = plan.facts().memory_grant_feedback.as_ref()?;
        (plan.engine == Engine::Mssql
            && plan.kind == PlanKind::Actual
            && feedback.is_valid()
            && feedback.runtime_observed
            && feedback.state == PlanMemoryGrantFeedbackState::DisabledUnstable
            && plan.nodes().iter().any(|node| {
                node.act_rows.is_some_and(|rows| rows.is_finite() && rows >= 0.0)
                    && node.act_executions.is_some_and(|executions| {
                        executions.is_finite() && executions >= 1.0 && executions.fract() == 0.0
                    })
            }))
        .then_some(feedback)
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PlanMemoryGrantFeedback>, D::Error> {
    Ok(Option::<PlanMemoryGrantFeedback>::deserialize(deserializer)?.filter(PlanMemoryGrantFeedback::is_valid))
}

#[cfg(test)]
mod tests;
