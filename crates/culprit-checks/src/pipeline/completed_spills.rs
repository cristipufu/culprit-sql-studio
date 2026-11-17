//! Original cache-entry evidence is independent of the scan's aggregate workload population.
use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::CompletedSpillReport>> {
    snapshot.operations.cached_runtime.as_ref()?;
    let result = crate::completed_spills::explain_snapshot(snapshot).and_then(|report| {
        report.map(Box::new).ok_or_else(|| culprit_core::Error::invalid(t!("view.completed_spills.invalid")))
    });
    admit_explanation(
        snapshot,
        culprit_core::telemetry::CACHED_RUNTIME_READ_SOURCE.into(),
        culprit_core::message!("view.completed_spills.invalid"),
        result,
    )
}

#[cfg(test)]
mod tests;
