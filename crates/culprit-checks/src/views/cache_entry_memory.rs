//! Whole-entry footprint and exact native cache-key differences establish context, not harmful churn or reuse.
mod comparison;
mod set_options;
pub use comparison::{CacheEntryMemoryEvidence, compare};

use super::source_coverage;
use culprit_core::{
    CacheEntryContextDifference, CacheEntryMemoryExplanation, CacheEntryMemoryReport, Error, Metric, Result,
    SourceOutcome, SourceState, format, key, t,
    telemetry::{
        CacheEntryStatement, CollectionTarget, MAX_CACHE_ENTRY_STATEMENTS, MssqlCacheEntryMemory, NativeReadContext,
        ObservationBounds, OperationalFact, OperationalObservation, SourcePartition, canonical_time,
    },
};
use std::collections::BTreeSet;

pub fn explain(
    context: &NativeReadContext,
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
) -> Result<CacheEntryMemoryReport> {
    let entries = validate(context, observations, source)?;
    let mut limitations = vec![
        t!("view.cache_entry_memory.limit.selected"),
        t!("view.cache_entry_memory.limit.memory"),
        t!("view.cache_entry_memory.limit.lookups"),
        t!("view.cache_entry_memory.limit.continuity"),
    ];
    if !context.has_qualified_local_owner() {
        limitations.push(t!("view.cache_entry_memory.limit.owner"));
    }
    if entries.iter().any(|(_, entry)| !known_keys(entry)) {
        limitations.push(t!("view.cache_entry_memory.limit.keys"));
    }
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.cache_entry_memory.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.cache_entry_memory.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.cache_entry_memory.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    Ok(CacheEntryMemoryReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.cache_entry_memory.title"),
        summary: if entries.is_empty() {
            t!("view.cache_entry_memory.summary.empty")
        } else {
            t!(
                "view.cache_entry_memory.summary.entries",
                n = entries.len(),
                count = format::count(entries.len() as f64)
            )
        },
        limitations,
        context_differences: if context.has_qualified_local_owner() { differences(&entries) } else { vec![] },
        entries: entries.into_iter().map(|(observation, entry)| render(observation, entry)).collect(),
        comparison: None,
    })
}

type Entry<'a> = (&'a OperationalObservation, &'a MssqlCacheEntryMemory);

fn validate<'a>(
    context: &NativeReadContext,
    observations: &[&'a OperationalObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<Entry<'a>>> {
    if !context.is_valid()
        || !matches!(context.target, CollectionTarget::Database(_))
        || context.target.engine() != culprit_core::Engine::Mssql
        || !matches!(context.partition, SourcePartition::InstanceLocal { .. })
        || observations.len() > MAX_CACHE_ENTRY_STATEMENTS
        || !source_coverage::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let bounds = ObservationBounds::new(&context.observed_from, &context.observed_until).ok_or_else(invalid)?;
    let mut keys = BTreeSet::new();
    let mut handles = BTreeSet::new();
    let mut point = None;
    let mut entries = Vec::new();
    for observation in observations {
        let OperationalFact::MssqlCacheEntryMemory(entry) = &observation.fact else { return Err(invalid()) };
        let at = canonical_time(&entry.observed_at)?;
        if !observation.is_valid()
            || !bounds.contains(&entry.observed_at)
            || !keys.insert(&observation.key)
            || !handles.insert(entry.plan_handle.to_ascii_lowercase())
            || point.as_ref().is_some_and(|point| point != &at)
        {
            return Err(invalid());
        }
        point = Some(at);
        entries.push((*observation, entry.as_ref()));
    }
    entries.sort_by(|left, right| left.0.key.cmp(&right.0.key));
    Ok(entries)
}

fn render(observation: &OperationalObservation, entry: &MssqlCacheEntryMemory) -> CacheEntryMemoryExplanation {
    let (summary, investigation) = entry_words(entry);
    CacheEntryMemoryExplanation {
        observation_key: observation.key.clone(),
        title: t!("view.cache_entry_memory.entry.title", handle = &entry.plan_handle),
        explanation: format!(
            "{} {}",
            summary,
            t!(
                "view.cache_entry_memory.entry.statements",
                n = entry.statements.len(),
                count = format::count(entry.statements.len() as f64)
            )
        ),
        metrics: vec![
            Metric::said_text(key!("view.cache_entry_memory.metric.bytes"), format::digits(entry.size_bytes.clone())),
            Metric::said_text(
                key!("view.cache_entry_memory.metric.lookups"),
                format::digits(entry.lookup_count.clone()),
            ),
            Metric::said_text(
                key!("view.cache_entry_memory.metric.references"),
                format::digits(entry.reference_count.clone()),
            ),
        ],
        investigation,
        verify: t!("view.cache_entry_memory.verify"),
    }
}

fn entry_words(entry: &MssqlCacheEntryMemory) -> (String, String) {
    match (entry.cache_object_type.as_str(), entry.object_type.as_str()) {
        ("Compiled Plan Stub", _) => {
            (t!("view.cache_entry_memory.entry.stub"), t!("view.cache_entry_memory.investigate.stub"))
        }
        ("Compiled Plan", "Adhoc") => {
            (t!("view.cache_entry_memory.entry.adhoc"), t!("view.cache_entry_memory.investigate.adhoc"))
        }
        ("Compiled Plan", "Prepared") => {
            (t!("view.cache_entry_memory.entry.prepared"), t!("view.cache_entry_memory.investigate.prepared"))
        }
        ("Compiled Plan", _) => {
            (t!("view.cache_entry_memory.entry.module"), t!("view.cache_entry_memory.investigate.module"))
        }
        _ => (t!("view.cache_entry_memory.entry.other"), t!("view.cache_entry_memory.investigate.other")),
    }
}

fn known_keys(entry: &MssqlCacheEntryMemory) -> bool {
    !entry.cache_keys.is_empty()
        && entry.cache_keys.iter().all(|attribute| attribute.native_type.is_some() && attribute.value.is_some())
}

fn same_cache_keys_except(left: &MssqlCacheEntryMemory, right: &MssqlCacheEntryMemory, excluded: Option<&str>) -> bool {
    known_keys(left)
        && known_keys(right)
        && left.cache_keys.len() == right.cache_keys.len()
        && left.cache_keys.iter().all(|left| {
            right.cache_keys.iter().find(|right| left.name == right.name).is_some_and(|right| {
                left.native_type == right.native_type
                    && (excluded == Some(left.name.as_str()) || left.value == right.value)
            })
        })
}

fn differences(entries: &[Entry<'_>]) -> Vec<CacheEntryContextDifference> {
    let mut differences = Vec::new();
    for (index, (left_ref, left)) in entries.iter().enumerate() {
        for (right_ref, right) in &entries[index + 1..] {
            if !known_keys(left) || !known_keys(right) || !same_native_statement(left, right) {
                continue;
            }
            let attributes = left
                .cache_keys
                .iter()
                .filter_map(|left| {
                    let right = right.cache_keys.iter().find(|right| left.name == right.name)?;
                    (left.native_type != right.native_type || left.value != right.value).then(|| left.name.clone())
                })
                .collect::<Vec<_>>();
            if attributes.is_empty() {
                continue;
            }
            let (explanation, investigation) = set_options::select(left, right).map_or_else(
                || {
                    (
                        t!("view.cache_entry_memory.context.difference", attributes = attributes.join(", ")),
                        t!("view.cache_entry_memory.context.investigation"),
                    )
                },
                |difference| difference.render(),
            );
            differences.push(CacheEntryContextDifference {
                left_observation_key: left_ref.key.clone(),
                right_observation_key: right_ref.key.clone(),
                explanation,
                investigation,
                attributes,
            });
        }
    }
    differences
}

fn same_native_statement(left: &MssqlCacheEntryMemory, right: &MssqlCacheEntryMemory) -> bool {
    left.sql_handle.eq_ignore_ascii_case(&right.sql_handle)
        && left.statements.iter().any(|left| right.statements.iter().any(|right| same_native_range(left, right)))
}

fn same_native_range(left: &CacheEntryStatement, right: &CacheEntryStatement) -> bool {
    left.context.database_id == right.context.database_id
        && left.context.identity.statement_start_bytes == right.context.identity.statement_start_bytes
        && left.context.identity.statement_end_bytes == right.context.identity.statement_end_bytes
}

fn invalid() -> Error {
    Error::invalid(t!("view.cache_entry_memory.invalid"))
}

#[cfg(test)]
mod tests;
