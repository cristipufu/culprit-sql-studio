//! Shared source qualification does not interpret the native meaning of memory measurements.
use culprit_core::{SourceOutcome, SourceState, t};

pub(super) fn source_limitation(source: Option<&SourceOutcome>) -> Option<String> {
    match source.map(|source| source.state) {
        None => Some(t!("view.memory_participants.limit.source_unknown")),
        Some(SourceState::Partial) => Some(t!("view.memory_participants.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            Some(t!("view.memory_participants.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => None,
    }
}
