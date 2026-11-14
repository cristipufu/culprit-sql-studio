//! Task counters establish allocation/deallocation activity, not current footprint, spill time or an allocator.
use culprit_core::{
    Error, Result, SourceOutcome, SourceState, TaskSpaceExplanation, TaskSpaceReport, format, t,
    telemetry::{MAX_TASK_SPACE_OBSERVATIONS, TaskRequestContext, TaskSpaceObservation},
};
use std::collections::BTreeSet;

pub fn explain(observations: &[&TaskSpaceObservation], source: Option<&SourceOutcome>) -> Result<TaskSpaceReport> {
    let expected =
        source.map(|source| source.source.as_str()).or_else(|| observations.first().map(|row| row.source.as_str()));
    let inconsistent = source.is_some_and(|source| {
        if observations.is_empty() {
            source.state == SourceState::Collected
        } else {
            matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable)
        }
    });
    if observations.len() > MAX_TASK_SPACE_OBSERVATIONS || inconsistent {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let entries = observations
        .iter()
        .map(|row| {
            if !row.is_valid() || expected != Some(row.source.as_str()) {
                return Err(invalid());
            }
            let key = row.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            Ok(TaskSpaceExplanation {
                observation_key: key,
                task: row.task.clone(),
                title: t!(
                    "view.task_space.entry.title",
                    session = row.task.session_id.as_str(),
                    request = row.task.request_id.as_str(),
                    task = row.task.execution_context_id.as_str()
                ),
                explanation: format!("{} {}", t!("view.task_space.entry.summary"), context(row.request_context())),
                investigation: t!("view.task_space.investigation"),
                verify: t!("view.task_space.verify"),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut limitations = vec![
        t!("view.task_space.limit.counters"),
        t!("view.task_space.limit.context"),
        t!("view.task_space.limit.non_atomic"),
    ];
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.task_space.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.task_space.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.task_space.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    Ok(TaskSpaceReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.task_space.title"),
        summary: if observations.is_empty() {
            t!("view.task_space.summary.empty")
        } else {
            t!(
                "view.task_space.summary.observed",
                n = observations.len(),
                count = format::count(observations.len() as f64)
            )
        },
        limitations,
        entries,
    })
}
fn context(state: TaskRequestContext) -> String {
    match state {
        TaskRequestContext::NativeJoinOnly => t!("view.task_space.context.join_only"),
        TaskRequestContext::CheckedAtBounds => t!("view.task_space.context.checked"),
        TaskRequestContext::Changed => t!("view.task_space.context.changed"),
        TaskRequestContext::Missing => t!("view.task_space.context.missing"),
        TaskRequestContext::Unverified => t!("view.task_space.context.unverified"),
    }
}
fn invalid() -> Error {
    Error::invalid(t!("collection.invalid_source"))
}

#[cfg(test)]
mod tests;
