//! Shared source qualification does not interpret the native meaning of memory measurements.
use culprit_core::{SourceOutcome, SourceState, t};

pub(super) fn consistent_source<'a>(
    source: Option<&SourceOutcome>,
    native_sources: impl IntoIterator<Item = &'a str>,
) -> bool {
    let mut names = native_sources.into_iter();
    let first = names.next();
    let incompatible = source.is_some_and(|source| match source.state {
        SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable => first.is_some(),
        SourceState::Collected => first.is_none(),
        SourceState::Partial => false,
    });
    let expected = source.map(|source| source.source.as_str()).or(first);
    !incompatible && first.is_none_or(|name| Some(name) == expected) && names.all(|name| Some(name) == expected)
}

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
