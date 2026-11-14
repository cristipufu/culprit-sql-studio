//! Pure views admit rows only from one exact source with a compatible native outcome.
use culprit_core::{SourceOutcome, SourceState};

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
