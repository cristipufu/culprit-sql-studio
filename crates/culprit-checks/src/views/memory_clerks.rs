//! Grouped native allocations provide context, not grants, memory pressure, a leak or a query owner.
use culprit_core::{
    Error, MeasurementScope, MemoryClerkEntry, MemoryClerkField, MemoryClerkReport, Result, Snapshot, SourceOutcome,
    SourceState, format, t,
    telemetry::{
        CapturedOperationalRead, CollectionBudget, CounterNature, CounterUnit, MEMORY_CLERK_NATIVE_OBJECT,
        MeasurementValue, NativeCounterSet, NativeReadContext, ObservationReadContext, OperationalFact,
        OperationalObservation, StateSourceCoverage, digest, memory_clerk_candidate, memory_clerk_source_is_valid,
    },
};
use std::collections::BTreeSet;

pub const MAX_MEMORY_CLERK_ENTRIES: usize = 256;

/// Acquisition domains remain explicit; a saved snapshot cannot invent original read clocks or counts.
pub enum MemoryClerkEvidence<'a> {
    Captured {
        context: &'a NativeReadContext,
        coverage: &'a StateSourceCoverage,
        observations: &'a [&'a OperationalObservation],
        read_complete: bool,
    },
    Snapshot {
        samples: &'a [&'a NativeCounterSet],
        source: Option<&'a SourceOutcome>,
    },
}

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<MemoryClerkReport>> {
    let samples =
        snapshot.pressure.as_ref().and_then(|pressure| pressure.resource_counters.as_deref()).unwrap_or_default();
    let samples = samples.iter().filter(|sample| memory_clerk_candidate(sample)).collect::<Vec<_>>();
    if let Some(captured) = snapshot.operations.memory_clerks.as_deref() {
        if !samples.is_empty() {
            return Err(invalid());
        }
        return explain_captured(captured).map(Some);
    }
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::MEMORY_CLERKS);
    if samples.is_empty() && source.is_none() {
        return Ok(None);
    }
    explain(MemoryClerkEvidence::Snapshot { samples: &samples, source }).map(Some)
}

/// Interpret one original shared capture. Structural binding is separate from the input owner's authentication.
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<MemoryClerkReport> {
    let coverage = read.memory_clerk_source()?;
    let context = read.context()?;
    let observations = read.observations.iter().filter(|row| row.source == coverage.source).collect::<Vec<_>>();
    explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage,
        observations: &observations,
        read_complete: read.read_complete,
    })
}

pub fn explain(evidence: MemoryClerkEvidence<'_>) -> Result<MemoryClerkReport> {
    match evidence {
        MemoryClerkEvidence::Captured { context, coverage, observations, read_complete } => {
            if !context.is_valid()
                || !memory_clerk_source_is_valid(coverage)
                || observations.len() > coverage.records as usize
                || read_complete && observations.len() != coverage.records as usize
            {
                return Err(invalid());
            }
            let mut samples = Vec::with_capacity(observations.len());
            for observation in observations {
                if !observation.is_valid() || observation.source != coverage.source {
                    return Err(invalid());
                }
                let OperationalFact::CounterSet(sample) = &observation.fact else { return Err(invalid()) };
                samples.push(sample.as_ref());
            }
            let missing = !read_complete
                && matches!(coverage.state, SourceState::Collected | SourceState::Empty | SourceState::Partial);
            let source = SourceOutcome {
                source: coverage.source.clone(),
                state: if missing { SourceState::Partial } else { coverage.state },
                reason: if missing { Some(t!("collected.memory_clerks.limit.read")) } else { source_reason(coverage) },
                reason_message: None,
            };
            let mut report = render(&samples, Some(&source), Some(read_complete))?;
            report.context = Some(ObservationReadContext::Native { context: Box::new(context.clone()) });
            report.native_source = Some(coverage.clone());
            report.native_rows = Some(coverage.records);
            if !context.has_qualified_local_owner() {
                report.limitations.push(t!("collected.memory_clerks.limit.owner"));
            }
            Ok(report)
        }
        MemoryClerkEvidence::Snapshot { samples, source } => render(samples, source, None),
    }
}

fn render(
    samples: &[&NativeCounterSet],
    source: Option<&SourceOutcome>,
    read_complete: Option<bool>,
) -> Result<MemoryClerkReport> {
    if samples.len() > CollectionBudget::upper_limits().max_snapshot_rows as usize
        || source.is_some_and(|source| source.source != culprit_core::source_id::MEMORY_CLERKS)
        || !super::source_coverage::consistent_source(
            source,
            samples.iter().map(|_| culprit_core::source_id::MEMORY_CLERKS),
        )
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let groups = samples.iter().map(|sample| validate(sample)).collect::<Result<Vec<_>>>()?;
    for group in &groups {
        if !keys.insert(&group.observation_key) {
            return Err(invalid());
        }
    }
    let entries = groups.iter().take(MAX_MEMORY_CLERK_ENTRIES).map(entry).collect::<Vec<_>>();
    let entries_limited = entries.len() < samples.len();
    let partial = read_complete == Some(false) || source.is_some_and(|source| source.state == SourceState::Partial);
    let unavailable =
        source.is_some_and(|source| matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable));
    let mut limitations = vec![
        t!("collected.memory_clerks.limit.scope"),
        t!("collected.memory_clerks.limit.dimensions"),
        t!("collected.memory_clerks.limit.cause"),
        t!("collected.memory_clerks.limit.clock"),
        t!("collected.memory_clerks.investigation"),
        t!("collected.memory_clerks.verify"),
    ];
    if read_complete.is_none() {
        limitations.push(t!("collected.memory_clerks.limit.original"));
    }
    if source.is_none() {
        limitations.push(t!("collected.memory_clerks.limit.coverage"));
    }
    if read_complete == Some(false) {
        limitations.push(t!("collected.memory_clerks.limit.read"));
    }
    if partial {
        limitations.push(t!("collected.memory_clerks.limit.partial"));
    }
    if entries_limited {
        limitations.push(t!("collected.memory_clerks.limit.presentation"));
    }
    Ok(MemoryClerkReport {
        context: None,
        native_source: None,
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        read_complete,
        native_rows: None,
        observed_rows: samples.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited,
        title: t!("collected.memory_clerks.title"),
        summary: if unavailable {
            t!("collected.memory_clerks.summary.unavailable")
        } else if partial {
            t!("collected.memory_clerks.summary.partial")
        } else if samples.is_empty() {
            t!("collected.memory_clerks.summary.empty")
        } else {
            t!("collected.memory_clerks.summary.observed", count = format::digits(samples.len().to_string()))
        },
        limitations,
        entries,
    })
}

struct Field {
    key: &'static str,
    native_name: &'static str,
    unit: CounterUnit,
    label: fn() -> String,
}
const FIELDS: &[Field] = &[
    Field {
        key: "clerk_count",
        native_name: "COUNT_BIG(*)",
        unit: CounterUnit::Count,
        label: || t!("collected.memory_clerks.field.clerk_count"),
    },
    Field {
        key: "pages_kb",
        native_name: "pages_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.pages_kb"),
    },
    Field {
        key: "virtual_memory_reserved_kb",
        native_name: "virtual_memory_reserved_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.virtual_memory_reserved_kb"),
    },
    Field {
        key: "virtual_memory_committed_kb",
        native_name: "virtual_memory_committed_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.virtual_memory_committed_kb"),
    },
    Field {
        key: "awe_allocated_kb",
        native_name: "awe_allocated_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.awe_allocated_kb"),
    },
    Field {
        key: "shared_memory_reserved_kb",
        native_name: "shared_memory_reserved_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.shared_memory_reserved_kb"),
    },
    Field {
        key: "shared_memory_committed_kb",
        native_name: "shared_memory_committed_kb",
        unit: CounterUnit::Kibibytes,
        label: || t!("collected.memory_clerks.field.shared_memory_committed_kb"),
    },
    Field {
        key: "minimum_page_size_in_bytes",
        native_name: "page_size_in_bytes",
        unit: CounterUnit::Bytes,
        label: || t!("collected.memory_clerks.field.minimum_page_size_in_bytes"),
    },
    Field {
        key: "maximum_page_size_in_bytes",
        native_name: "page_size_in_bytes",
        unit: CounterUnit::Bytes,
        label: || t!("collected.memory_clerks.field.maximum_page_size_in_bytes"),
    },
];

struct Group<'a> {
    sample: &'a NativeCounterSet,
    observation_key: String,
    clerk_type: &'a str,
    name: &'a str,
    node: &'a str,
}

fn validate(sample: &NativeCounterSet) -> Result<Group<'_>> {
    let get = |key: &str| sample.properties.get(key).map(String::as_str).ok_or_else(invalid);
    let kind = get("type")?;
    let name = get("name")?;
    let node = get("memory_node_id")?;
    let count =
        sample.counters.iter().find(|counter| counter.key == "clerk_count").and_then(|counter| counter.value.integer());
    if !sample.is_valid()
        || sample.scope != MeasurementScope::Server
        || !sample.ratios.is_empty()
        || !sample.ratio_gaps.is_empty()
        || sample.identity.qualifiers.len() != 3
        || sample.properties.len() != 4
        || sample.identity.id != kind
        || kind.is_empty()
        || kind.chars().count() > 60
        || name.encode_utf16().count() > 256
        || node.parse::<i16>().ok().is_none_or(|value| value.to_string() != node)
        || sample.identity.qualifiers.get("native_object").map(String::as_str) != Some(MEMORY_CLERK_NATIVE_OBJECT)
        || sample.identity.qualifiers.get("memory_node_id").map(String::as_str) != Some(node)
        || sample.identity.qualifiers.get("name_sha256") != Some(&digest(name.as_bytes()))
        || get("aggregation")? != "clerk-type-name-and-memory-node"
        || count.is_none_or(|value| value == 0)
    {
        return Err(invalid());
    }
    for counter in &sample.counters {
        let Some(field) = FIELDS.iter().find(|field| field.key == counter.key) else { return Err(invalid()) };
        if counter.unit != field.unit
            || counter.nature != CounterNature::Gauge
            || counter.scope != MeasurementScope::Server
            || counter.value.integer().is_none()
            || counter.native_object != MEMORY_CLERK_NATIVE_OBJECT
            || counter.native_name != field.native_name
            || counter.native_instance.is_some()
            || counter.native_type.is_some()
            || counter.reset != culprit_core::ResetMarker::Unknown
        {
            return Err(invalid());
        }
    }
    let value = |key: &str| {
        sample.counters.iter().find(|counter| counter.key == key).and_then(|counter| counter.value.integer())
    };
    if value("minimum_page_size_in_bytes")
        .zip(value("maximum_page_size_in_bytes"))
        .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(invalid());
    }
    Ok(Group { sample, observation_key: sample.key()?, clerk_type: kind, name, node })
}

fn entry(group: &Group<'_>) -> MemoryClerkEntry {
    let sample = group.sample;
    MemoryClerkEntry {
        observation_key: group.observation_key.clone(),
        scope: sample.scope,
        clerk_type: group.clerk_type.to_owned(),
        name: group.name.to_owned(),
        memory_node_id: group.node.to_owned(),
        explanation: t!("collected.memory_clerks.entry.explanation"),
        fields: FIELDS
            .iter()
            .map(|field| MemoryClerkField {
                key: field.key.into(),
                label: (field.label)(),
                unit: field.unit,
                value: sample.counters.iter().find(|counter| counter.key == field.key).and_then(
                    |counter| match &counter.value {
                        MeasurementValue::Integer(value) => Some(value.clone()),
                        _ => None,
                    },
                ),
            })
            .collect(),
    }
}

fn source_reason(source: &StateSourceCoverage) -> Option<String> {
    if let Some(stop) = source.stopped {
        Some(stop.error().message)
    } else if source.error_code.is_some() {
        Some(t!("collected.memory_clerks.source.unavailable"))
    } else if source.state == SourceState::Partial {
        Some(t!("collected.memory_clerks.limit.partial"))
    } else {
        None
    }
}
fn invalid() -> Error {
    Error::invalid(t!("collected.memory_clerks.invalid"))
}

#[cfg(test)]
mod tests;
