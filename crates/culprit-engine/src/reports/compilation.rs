//! One previous query-global compiler read, never a sum over runtime-population reports.
use super::*;
use culprit_core::telemetry::CompilationBaseline;

const MAX_BASELINE_READS: usize = 32;

pub(crate) fn apply(dir: &Path, snapshot: &mut culprit_core::Snapshot, immediate: Option<&culprit_core::Snapshot>) {
    if snapshot.operations.query_compilation.is_none() {
        return;
    }
    if let Some(previous) = immediate.filter(|previous| eligible_snapshot(previous, snapshot))
        && let Some(baseline) = CompilationBaseline::from_snapshot(previous)
    {
        snapshot.compilations_before = Some(baseline);
        return;
    }
    let Ok(history) = list(dir) else {
        return;
    };
    for summary in history
        .into_iter()
        .filter(|summary| {
            summary.id != snapshot.id
                && summary.workload_population.is_none()
                && predates(&summary.taken_at, &snapshot.taken_at)
        })
        .take(MAX_BASELINE_READS)
    {
        let Ok(previous) = load_snapshot(&dir.join(format!("{}.json", summary.id))) else {
            continue;
        };
        if !eligible_snapshot(&previous, snapshot) {
            continue;
        }
        if let Some(baseline) = CompilationBaseline::from_snapshot(&previous) {
            snapshot.compilations_before = Some(baseline);
            return;
        }
    }
    // A retained baseline survives pruning of its original report. Its native authority is rechecked by the view.
}

fn eligible_snapshot(previous: &culprit_core::Snapshot, current: &culprit_core::Snapshot) -> bool {
    previous.id != current.id
        && previous.options.as_ref().is_none_or(|options| options.workload_population.is_none())
        && predates(&previous.taken_at, &current.taken_at)
}

fn predates(before: &str, after: &str) -> bool {
    culprit_core::telemetry::canonical_time(before)
        .ok()
        .zip(culprit_core::telemetry::canonical_time(after).ok())
        .is_some_and(|(before, after)| before < after)
}

#[cfg(test)]
mod tests;
