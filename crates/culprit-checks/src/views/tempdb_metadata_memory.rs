//! Tempdb's native metadata-memory gauges distinguish reservation from usage without a pressure or query verdict.
use culprit_core::{
    ErrorCode, Result, SourceState, TempdbMetadataMemoryEntry, TempdbMetadataMemoryReport, t, telemetry::*,
};
use std::collections::BTreeSet;

pub struct TempdbMetadataMemoryEvidence<'a> {
    pub context: &'a NativeReadContext,
    pub coverage: &'a StateSourceCoverage,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}

pub fn explain(input: TempdbMetadataMemoryEvidence<'_>) -> Result<TempdbMetadataMemoryReport> {
    let invalid = || CollectionStop::InvalidPage.error();
    if !tempdb_metadata_memory_owner_is_qualified(input.context)
        || !tempdb_metadata_memory_source_is_valid(input.coverage)
        || input.observations.len() > input.coverage.records as usize
        || input.read_complete && input.observations.len() != input.coverage.records as usize
        || !input.observations.is_empty()
            && !matches!(input.coverage.state, SourceState::Collected | SourceState::Partial)
    {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    let mut native_point = None;
    let mut profile = None;
    let mut database_created = None;
    let mut entries = Vec::new();
    for row in input.observations {
        if !row.is_valid() || row.source != input.coverage.source {
            return Err(invalid());
        }
        let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
        if row.key != sample.key()? {
            return Err(invalid());
        }
        if !seen.insert(sample.identity.clone()) {
            return Err(invalid());
        }
        if sample.identity.qualifiers.get("kind").map(String::as_str) == Some(MSSQL_TEMPDB_METADATA_FEATURE_KIND) {
            if profile.is_some() {
                return Err(invalid());
            }
            profile = Some(TempdbMetadataFeature::from_original(sample, input.context)?);
            continue;
        }
        let native = TempdbMetadataConsumer::from_original(sample, input.context)?;
        if native_point.is_some_and(|at| at != native.observed_at())
            || database_created.is_some_and(|created| created != native.database_created())
        {
            return Err(invalid());
        }
        native_point = Some(native.observed_at());
        database_created = Some(native.database_created());
        entries.push(entry(row, &native)?);
    }
    if input.read_complete && !input.observations.is_empty() && profile.is_none() {
        return Err(invalid());
    }
    if let (Some(profile), Some(point), Some(created)) = (&profile, native_point, database_created) {
        if profile.enabled() != Some(true)
            || profile.database_created() != created
            || ObservationBounds::new(profile.observed_at(), point).is_none()
        {
            return Err(invalid());
        }
    }
    let metadata_enabled =
        profile.as_ref().and_then(TempdbMetadataFeature::enabled).or_else(|| (!entries.is_empty()).then_some(true));
    let source_state =
        if !input.read_complete && matches!(input.coverage.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            input.coverage.state
        };
    let mut limitations = vec![
        t!("view.tempdb_metadata_memory.limit.scope"),
        t!("view.tempdb_metadata_memory.limit.point"),
        t!("view.tempdb_metadata_memory.limit.owner"),
    ];
    if source_state == SourceState::Partial {
        limitations.push(t!("view.tempdb_metadata_memory.limit.partial"));
    }
    if let Some(reason) = source_limitation(input.coverage) {
        limitations.push(reason);
    }
    Ok(TempdbMetadataMemoryReport {
        context: ObservationReadContext::Native { context: Box::new(input.context.clone()) },
        native_source: input.coverage.clone(),
        source_state,
        read_complete: input.read_complete,
        metadata_enabled,
        feature_observed_at: profile.as_ref().map(|profile| profile.observed_at().to_owned()),
        tempdb_created_without_offset: profile
            .as_ref()
            .map(|profile| profile.database_created())
            .or(database_created)
            .map(str::to_owned),
        native_rows: input.coverage.records,
        observed_rows: input.observations.len().try_into().map_err(|_| invalid())?,
        title: t!("view.tempdb_metadata_memory.title"),
        summary: if metadata_enabled == Some(false) {
            t!("view.tempdb_metadata_memory.summary.disabled")
        } else if entries.is_empty() {
            t!("view.tempdb_metadata_memory.summary.unavailable")
        } else {
            t!("view.tempdb_metadata_memory.summary.recorded")
        },
        limitations,
        entries,
    })
}

fn source_limitation(source: &StateSourceCoverage) -> Option<String> {
    if let Some(stop) = source.stopped {
        return Some(stop.error().message);
    }
    source.error_code.map(|error| match error {
        ErrorCode::PermissionDenied => t!("view.tempdb_metadata_memory.source.denied"),
        ErrorCode::Unsupported => t!("view.tempdb_metadata_memory.source.unsupported"),
        _ => t!("view.tempdb_metadata_memory.source.failed"),
    })
}

fn entry(row: &OperationalObservation, native: &TempdbMetadataConsumer<'_>) -> Result<TempdbMetadataMemoryEntry> {
    let sample = native.sample();
    let explanation = if !native.usage_is_applicable() {
        t!("view.tempdb_metadata_memory.usage.not_applicable")
    } else {
        match (native.allocated_bytes(), native.used_bytes()) {
            (Some(allocated), Some(used)) => {
                let allocated = allocated.parse::<u64>().map_err(|_| CollectionStop::InvalidPage.error())?;
                let used = used.parse::<u64>().map_err(|_| CollectionStop::InvalidPage.error())?;
                if used > allocated {
                    t!("view.tempdb_metadata_memory.usage.inconsistent")
                } else if allocated == 0 {
                    t!("view.tempdb_metadata_memory.usage.zero")
                } else if used < allocated {
                    t!("view.tempdb_metadata_memory.usage.reservation")
                } else {
                    t!("view.tempdb_metadata_memory.usage.used")
                }
            }
            _ => t!("view.tempdb_metadata_memory.usage.unknown"),
        }
    };
    Ok(TempdbMetadataMemoryEntry {
        observation_key: row.key.clone(),
        observed_at: native.observed_at().into(),
        native_consumer_id: sample.identity.id.clone(),
        native_type_code: native.native_type().to_string(),
        native_type_name: sample.properties["memory_consumer_type_desc"].clone(),
        description: sample.properties.get("memory_consumer_desc").cloned(),
        allocated_bytes: native.allocated_bytes().map(str::to_owned),
        used_bytes: native.used_bytes().map(str::to_owned),
        explanation,
        investigation: t!("view.tempdb_metadata_memory.investigate"),
        verify: t!("view.tempdb_metadata_memory.verify"),
    })
}

#[cfg(test)]
mod tests;
