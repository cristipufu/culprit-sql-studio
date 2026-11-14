//! Admit independent native cleanup reads without replacing the captured evidence or inventing coverage.
use crate::cleanup::{self, CleanupEvidence};
use culprit_core::{
    CleanupReport, CollectorNote, Snapshot, SourceOutcome, SourceState,
    telemetry::{OperationalFact, OperationalObservation, RetentionCoverage, RetentionSnapshot},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<CleanupReport> {
    let original_retention = snapshot.pressure.as_ref().and_then(|pressure| pressure.retention.as_ref());
    if original_retention.is_none() && snapshot.operations.cleanup.is_empty() {
        return None;
    }
    let mut sources = selected_sources(snapshot);
    let mut invalid = BTreeSet::new();
    let retention =
        original_retention.filter(|population| retention_is_usable(population, &snapshot.server.database, &sources));
    let recovered_holders = if retention.is_none() {
        original_retention
            .map(|population| {
                invalid.insert(RetentionCoverage::SOURCE.to_string());
                usable_holders(population, &snapshot.server.database, &mut invalid)
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut native = native_points(snapshot, &mut invalid);
    native.extend(recovered_holders.iter());
    reject_source_contradictions(snapshot, &sources, &mut native, &mut invalid);
    for source in &invalid {
        let positive = native.iter().any(|row| row.source == *source);
        withhold_invalid_source(&mut sources, source, positive);
    }
    let explained = cleanup::explain(CleanupEvidence { retention, native: &native }, &sources);
    let mut report = match explained {
        Ok(report) => Some(report),
        Err(_) => {
            invalid.insert("cleanup".into());
            None
        }
    };
    if !invalid.is_empty() {
        if let Some(report) = &mut report {
            report.limitations.push(culprit_core::t!("view.cleanup.invalid"));
        }
        record_invalid_inputs(snapshot, invalid);
    }
    report
}

fn selected_sources(snapshot: &Snapshot) -> Vec<SourceOutcome> {
    let population = snapshot.pressure.as_ref().and_then(|pressure| pressure.retention.as_ref());
    let mut names = snapshot.operations.cleanup.iter().map(|read| read.source.as_str()).collect::<BTreeSet<_>>();
    if let Some(population) = population {
        names.extend(population.sources.iter().map(|source| source.source.as_str()));
        names.extend(population.coverage.native_sources.iter().map(String::as_str));
        names.extend(population.holders.iter().map(|holder| holder.source.as_str()));
    }
    let mut sources = snapshot
        .sources
        .iter()
        .filter(|source| names.contains(source.source.as_str()) || cleanup::is_cleanup_source(&source.source))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(population) = population {
        for source in &population.sources {
            if !sources.iter().any(|outcome| outcome.source == source.source) {
                sources.push(SourceOutcome {
                    source: source.source.clone(),
                    state: source.state,
                    reason: None,
                    reason_message: None,
                });
            }
        }
    }
    sources
}

fn retention_is_usable(population: &RetentionSnapshot, database: &str, sources: &[SourceOutcome]) -> bool {
    population.is_valid()
        && population.holders.iter().all(|holder| holder.fits_database_capture(database))
        && population.sources.iter().all(|native| {
            let rows = population.holders.iter().filter(|holder| holder.source == native.source).collect::<Vec<_>>();
            let source = sources.iter().find(|source| source.source == native.source);
            cleanup::retention_source_is_consistent(source, &rows)
        })
}

fn usable_holders(
    population: &RetentionSnapshot,
    database: &str,
    invalid: &mut BTreeSet<String>,
) -> Vec<OperationalObservation> {
    let mut identities = BTreeMap::<_, Vec<_>>::new();
    for holder in &population.holders {
        if !holder.is_valid() || !holder.fits_database_capture(database) {
            invalid.insert(holder.source.clone());
            continue;
        }
        let Ok(key) = holder.key() else {
            invalid.insert(holder.source.clone());
            continue;
        };
        identities.entry((holder.source.as_str(), key)).or_default().push(holder);
    }
    identities
        .into_iter()
        .filter_map(|((source, key), rows)| {
            let [holder] = rows.as_slice() else {
                invalid.insert(source.to_string());
                return None;
            };
            Some(OperationalObservation {
                source: source.into(),
                key,
                fact: OperationalFact::Retention(Box::new((**holder).clone())),
            })
        })
        .collect()
}

fn native_points<'a>(snapshot: &'a Snapshot, invalid: &mut BTreeSet<String>) -> Vec<&'a OperationalObservation> {
    let mut rejected = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut points = Vec::new();
    for read in &snapshot.operations.cleanup {
        if !read.is_valid() {
            rejected.insert(read.source.clone());
            continue;
        }
        for row in &read.observations {
            if !keys.insert((&row.source, &row.key)) {
                rejected.insert(row.source.clone());
            }
            points.push(row);
        }
    }
    points.retain(|point| !rejected.contains(&point.source));
    invalid.extend(rejected);
    points
}

fn reject_source_contradictions(
    snapshot: &Snapshot,
    sources: &[SourceOutcome],
    native: &mut Vec<&OperationalObservation>,
    invalid: &mut BTreeSet<String>,
) {
    let names = snapshot
        .operations
        .cleanup
        .iter()
        .map(|read| read.source.as_str())
        .chain(native.iter().map(|row| row.source.as_str()))
        .collect::<BTreeSet<_>>();
    let rejected = names
        .into_iter()
        .filter(|name| {
            let rows = native.iter().copied().filter(|row| row.source == *name).collect::<Vec<_>>();
            !cleanup::native_source_is_consistent(sources.iter().find(|source| source.source == *name), &rows)
        })
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    native.retain(|row| !rejected.contains(&row.source));
    invalid.extend(rejected);
}

fn withhold_invalid_source(sources: &mut Vec<SourceOutcome>, name: &str, has_rows: bool) {
    let invalid = culprit_core::message!("view.cleanup.invalid");
    let state = if has_rows { SourceState::Partial } else { SourceState::Unavailable };
    if let Some(source) = sources.iter_mut().find(|source| source.source == name) {
        if matches!(source.state, SourceState::Collected | SourceState::Empty) {
            source.state = state;
            source.reason = Some(invalid.render());
            source.reason_message = Some(invalid);
        }
    } else {
        sources.push(SourceOutcome {
            source: name.into(),
            state,
            reason: Some(invalid.render()),
            reason_message: Some(invalid),
        });
    }
}

fn record_invalid_inputs(snapshot: &mut Snapshot, sources: BTreeSet<String>) {
    let message = culprit_core::message!("view.cleanup.invalid");
    for source in sources {
        if !snapshot.notes.iter().any(|note| note.collector == source && note.message_source.as_ref() == Some(&message))
        {
            snapshot.notes.push(CollectorNote {
                collector: source,
                message: message.render(),
                message_source: Some(message.clone()),
                table: None,
            });
        }
    }
}

#[cfg(test)]
mod tests;
