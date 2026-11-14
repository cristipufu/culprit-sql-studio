//! Task counters establish allocation/deallocation activity, not current footprint, spill time or an allocator.
use culprit_core::{
    Error, Result, SourceOutcome, SourceState, TaskAllocationActivity, TaskSpaceExplanation, TaskSpaceReport, format,
    t,
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
            let activity = allocation_activity(row);
            Ok(TaskSpaceExplanation {
                observation_key: key,
                task: row.task.clone(),
                activity,
                title: t!(
                    "view.task_space.entry.title",
                    session = row.task.session_id.as_str(),
                    request = row.task.request_id.as_str(),
                    task = row.task.execution_context_id.as_str()
                ),
                explanation: format!(
                    "{} {} {}",
                    activity_summary(activity),
                    t!("view.task_space.entry.summary"),
                    context(row.request_context())
                ),
                investigation: investigation(activity),
                verify: t!("view.task_space.verify"),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut limitations = vec![
        t!("view.task_space.limit.counters"),
        t!("view.task_space.limit.context"),
        t!("view.task_space.limit.non_atomic"),
    ];
    if observations.iter().any(|row| counters(row).into_iter().any(Option::is_none)) {
        limitations.push(t!("view.task_space.limit.incomplete_counters"));
    }
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

fn allocation_activity(row: &TaskSpaceObservation) -> TaskAllocationActivity {
    let positive = |counts: [&Option<String>; 2]| {
        counts.into_iter().flatten().any(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
    };
    let user = positive([&row.user_allocated_pages, &row.user_deallocated_pages]);
    let internal = positive([&row.internal_allocated_pages, &row.internal_deallocated_pages]);
    match (user, internal) {
        (true, true) => TaskAllocationActivity::Mixed,
        (true, false) => TaskAllocationActivity::UserObjects,
        (false, true) => TaskAllocationActivity::InternalObjects,
        (false, false) if counters(row).into_iter().all(Option::is_some) => TaskAllocationActivity::NoRecordedActivity,
        (false, false) => TaskAllocationActivity::Unknown,
    }
}

fn counters(row: &TaskSpaceObservation) -> [&Option<String>; 4] {
    [
        &row.user_allocated_pages,
        &row.user_deallocated_pages,
        &row.internal_allocated_pages,
        &row.internal_deallocated_pages,
    ]
}

fn activity_summary(activity: TaskAllocationActivity) -> String {
    match activity {
        TaskAllocationActivity::UserObjects => t!("view.task_space.activity.user"),
        TaskAllocationActivity::InternalObjects => t!("view.task_space.activity.internal"),
        TaskAllocationActivity::Mixed => t!("view.task_space.activity.mixed"),
        TaskAllocationActivity::NoRecordedActivity => t!("view.task_space.activity.zero"),
        TaskAllocationActivity::Unknown => t!("view.task_space.activity.unknown"),
    }
}

fn investigation(activity: TaskAllocationActivity) -> String {
    match activity {
        TaskAllocationActivity::UserObjects => t!("view.task_space.investigate.user"),
        TaskAllocationActivity::InternalObjects => t!("view.task_space.investigate.internal"),
        TaskAllocationActivity::Mixed => t!("view.task_space.investigate.mixed"),
        TaskAllocationActivity::NoRecordedActivity => t!("view.task_space.investigate.zero"),
        TaskAllocationActivity::Unknown => t!("view.task_space.investigate.unknown"),
    }
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
