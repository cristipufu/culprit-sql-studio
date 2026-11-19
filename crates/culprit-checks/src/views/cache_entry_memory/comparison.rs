//! Compare complete supplied selections. A newly observed context is not a cache creation or a harmful variant.
use super::*;
use culprit_core::{CacheEntryPopulationComparison, CacheEntryRetainedChange};

pub struct CacheEntryMemoryEvidence<'a> {
    pub context: &'a NativeReadContext,
    pub observations: &'a [&'a OperationalObservation],
    pub source: Option<&'a SourceOutcome>,
    /// Archive completeness is independent of the native source's intentionally selected population.
    pub read_complete: bool,
}

pub fn compare(
    previous: CacheEntryMemoryEvidence<'_>,
    current: CacheEntryMemoryEvidence<'_>,
) -> Result<CacheEntryMemoryReport> {
    let before = validate(previous.context, previous.observations, previous.source)?;
    let after = validate(current.context, current.observations, current.source)?;
    let mut report = explain(current.context, current.observations, current.source)?;
    if let Some(reason) = refusal(&previous, &current) {
        report.limitations.push(reason.words());
        return Ok(report);
    }
    let anchors = retained_entries(&before, &after)?;
    if anchors.is_empty() {
        report.limitations.push(t!("view.cache_entry_memory.comparison.unavailable_anchor"));
        return Ok(report);
    }
    let bytes_before = selected_bytes(&before)?;
    let bytes_after = selected_bytes(&after)?;
    let mut limitations =
        vec![t!("view.cache_entry_memory.comparison.limit.selected"), t!("view.cache_entry_memory.limit.lookups")];
    if before.iter().any(|(_, entry)| !known_keys(entry)) || after.iter().any(|(_, entry)| !known_keys(entry)) {
        limitations.push(t!("view.cache_entry_memory.limit.keys"));
    }
    let additional_contexts = additional_contexts(&before, &after, &anchors);
    let retained_entries = anchors.into_iter().map(render_retained).collect();
    report.comparison = Some(Box::new(CacheEntryPopulationComparison {
        title: t!("view.cache_entry_memory.comparison.title"),
        summary: t!(
            "view.cache_entry_memory.comparison.summary",
            before_entries = format::digits(before.len().to_string()),
            after_entries = format::digits(after.len().to_string()),
            before_bytes = format::digits(bytes_before.clone()),
            after_bytes = format::digits(bytes_after.clone())
        ),
        entries_before: before.len() as u32,
        entries_after: after.len() as u32,
        bytes_before,
        bytes_after,
        limitations,
        retained_entries,
        additional_contexts,
    }));
    Ok(report)
}

enum Refusal {
    Incomplete,
    Source,
    Scope,
    Order,
}

impl Refusal {
    fn words(self) -> String {
        match self {
            Self::Incomplete => t!("view.cache_entry_memory.comparison.unavailable_read"),
            Self::Source => t!("view.cache_entry_memory.comparison.unavailable_source"),
            Self::Scope => t!("view.cache_entry_memory.comparison.unavailable_scope"),
            Self::Order => t!("view.cache_entry_memory.comparison.unavailable_order"),
        }
    }
}

fn refusal(previous: &CacheEntryMemoryEvidence<'_>, current: &CacheEntryMemoryEvidence<'_>) -> Option<Refusal> {
    if !previous.read_complete || !current.read_complete {
        return Some(Refusal::Incomplete);
    }
    let readable = |source: Option<&SourceOutcome>| {
        source.is_some_and(|source| {
            matches!(source.state, SourceState::Collected | SourceState::Partial | SourceState::Empty)
        })
    };
    if !readable(previous.source)
        || !readable(current.source)
        || previous.source.map(|source| &source.source) != current.source.map(|source| &source.source)
    {
        return Some(Refusal::Source);
    }
    let (before, after) = (previous.context, current.context);
    if !after.matches_earlier_scope(before) {
        return Some(Refusal::Scope);
    }
    let ordered = matches!(
        (canonical_time(&before.observed_until), canonical_time(&after.observed_until)),
        (Ok(before), Ok(after)) if before < after
    );
    if !ordered {
        return Some(Refusal::Order);
    }
    None
}

struct Retained<'a> {
    before: Entry<'a>,
    after: Entry<'a>,
    before_bytes: u64,
    after_bytes: u64,
    lookup_delta: u64,
}

fn retained_entries<'a>(before: &[Entry<'a>], after: &[Entry<'a>]) -> Result<Vec<Retained<'a>>> {
    let mut entries = Vec::new();
    for previous in before {
        let Some(current) = after.iter().find(|current| same_incarnation(previous.1, current.1)) else { continue };
        if !same_cache_keys_except(previous.1, current.1, None) {
            continue;
        }
        let before_lookups = native_count(&previous.1.lookup_count)?;
        let after_lookups = native_count(&current.1.lookup_count)?;
        let Some(lookup_delta) = after_lookups.checked_sub(before_lookups) else { continue };
        entries.push(Retained {
            before: *previous,
            after: *current,
            before_bytes: native_count(&previous.1.size_bytes)?,
            after_bytes: native_count(&current.1.size_bytes)?,
            lookup_delta,
        });
    }
    Ok(entries)
}

fn native_count(value: &str) -> Result<u64> {
    value.parse().map_err(|_| invalid())
}

fn same_incarnation(left: &MssqlCacheEntryMemory, right: &MssqlCacheEntryMemory) -> bool {
    left.plan_handle.eq_ignore_ascii_case(&right.plan_handle)
        && left.sql_handle.eq_ignore_ascii_case(&right.sql_handle)
        && left.cache_object_type == right.cache_object_type
        && left.object_type == right.object_type
        && set_options::same_original_selection(left, right)
        && left.statements.iter().all(|left| {
            left.statement.is_available()
                && right
                    .statements
                    .iter()
                    .any(|right| right.statement == left.statement && right.context == left.context)
        })
}

fn selected_bytes(entries: &[Entry<'_>]) -> Result<String> {
    entries
        .iter()
        .try_fold(0u128, |sum, (_, entry)| {
            let bytes = entry.size_bytes.parse::<u64>().map_err(|_| invalid())?;
            sum.checked_add(u128::from(bytes)).ok_or_else(invalid)
        })
        .map(|bytes| bytes.to_string())
}

fn additional_contexts(
    before: &[Entry<'_>],
    after: &[Entry<'_>],
    anchors: &[Retained<'_>],
) -> Vec<CacheEntryContextDifference> {
    after
        .iter()
        .filter_map(|(current_ref, current)| {
            if before.iter().any(|(_, previous)| {
                same_incarnation(previous, current)
                    || same_native_statement(previous, current)
                        && (!known_keys(previous)
                            || !set_options::same_original_selection(previous, current)
                            || same_cache_keys_except(previous, current, None))
            }) {
                return None;
            }
            let (anchor, difference) = anchors.iter().find_map(|anchor| {
                if !set_options::same_original_selection(anchor.before.1, current) {
                    return None;
                }
                set_options::select(anchor.before.1, current).map(|difference| (anchor.before, difference))
            })?;
            let (explanation, investigation) = difference.render();
            Some(CacheEntryContextDifference {
                left_observation_key: anchor.0.key.clone(),
                right_observation_key: current_ref.key.clone(),
                attributes: vec!["set_options".into()],
                explanation: format!("{} {}", t!("view.cache_entry_memory.comparison.additional"), explanation),
                investigation,
            })
        })
        .collect()
}

fn render_retained(entry: Retained<'_>) -> CacheEntryRetainedChange {
    let Retained { before, after, before_bytes, after_bytes, lookup_delta } = entry;
    CacheEntryRetainedChange {
        baseline_observation_key: before.0.key.clone(),
        observation_key: after.0.key.clone(),
        metrics: vec![
            Metric::said_text(
                key!("view.cache_entry_memory.comparison.metric.byte_change"),
                format::digits((i128::from(after_bytes) - i128::from(before_bytes)).to_string()),
            ),
            Metric::said_text(
                key!("view.cache_entry_memory.comparison.metric.lookup_change"),
                format::digits(lookup_delta.to_string()),
            ),
        ],
        explanation: t!(
            "view.cache_entry_memory.comparison.retained",
            handle = &after.1.plan_handle,
            before_bytes = format::digits(before.1.size_bytes.clone()),
            after_bytes = format::digits(after.1.size_bytes.clone()),
            lookups = format::digits(lookup_delta.to_string())
        ),
        investigation: entry_words(after.1).1,
        verify: t!("view.cache_entry_memory.verify"),
    }
}
