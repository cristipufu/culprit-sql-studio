//! Interpret only feedback attributes on the selected native QueryPlan; never borrow another statement's counters.
use super::children;
use culprit_core::plan_facts::{
    PlanMemoryGrantFeedback, PlanMemoryGrantFeedbackIssue as Issue, PlanMemoryGrantFeedbackState as State,
};
use roxmltree::Node;

const MAX_NATIVE_STATE_BYTES: usize = 128;

pub(super) fn read(query_plan: Node<'_, '_>) -> Option<PlanMemoryGrantFeedback> {
    let mut grants = children(query_plan, "MemoryGrantInfo");
    let grant = grants.next()?;
    if grants.next().is_some() {
        return Some(PlanMemoryGrantFeedback {
            state: State::Unrecognized,
            previous_grant_kb: None,
            runtime_observed: false,
            issues: vec![Issue::AmbiguousMetadata],
        });
    }
    let native_state = grant.attribute("IsMemoryGrantFeedbackAdjusted");
    let previous = grant.attribute("LastRequestedMemory");
    if native_state.is_none() && previous.is_none() {
        return None;
    }
    let mut issues = Vec::new();
    let state = match native_state {
        None => State::NotRecorded,
        Some("No: First Execution") => State::FirstExecution,
        Some("No: Accurate Grant") => State::AccurateGrant,
        Some("No: Feedback disabled") => State::DisabledUnstable,
        Some("Yes: Adjusting") => State::Adjusting,
        Some("Yes: Percentile Adjusting") => State::PercentileAdjusting,
        Some("Yes: Stable") => State::Stable,
        Some(value) => {
            if value.trim().is_empty() || value.len() > MAX_NATIVE_STATE_BYTES || value.chars().any(char::is_control) {
                issues.push(Issue::InvalidState);
            }
            State::Unrecognized
        }
    };
    let previous_grant_kb = previous.and_then(|value| {
        let number = (value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten();
        if number.is_none() {
            issues.push(Issue::InvalidPreviousGrant);
        }
        number.map(|number| number.to_string())
    });
    Some(PlanMemoryGrantFeedback {
        state,
        previous_grant_kb,
        runtime_observed: super::query_plan_has_runtime(query_plan),
        issues,
    })
}

#[cfg(test)]
mod tests;
