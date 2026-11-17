//! A positive observed group remains usable without becoming a complete replica inventory or an absence claim.
use culprit_core::{Snapshot, SourceOutcome, SourceState};

pub(super) fn qualify(snapshot: &mut Snapshot) {
    let catalog = snapshot
        .workload
        .as_ref()
        .and_then(|workload| workload.population_catalog.as_ref())
        .or(snapshot.runtime_populations.as_ref());
    let Some(catalog) = catalog.filter(|catalog| catalog.is_valid() && !catalog.has_complete_membership()) else {
        return;
    };
    let source = catalog.source.clone();
    let reason = culprit_core::message!("pipeline.note.runtime_population_observed");
    if let Some(outcome) = snapshot.sources.iter_mut().find(|outcome| outcome.source == source) {
        if matches!(outcome.state, SourceState::Collected | SourceState::Empty) {
            outcome.state = SourceState::Partial;
            outcome.reason = Some(reason.render());
            outcome.reason_message = Some(reason);
        }
    } else {
        snapshot.sources.push(SourceOutcome {
            source,
            state: SourceState::Partial,
            reason: Some(reason.render()),
            reason_message: Some(reason),
        });
    }
}

#[cfg(test)]
mod tests;
