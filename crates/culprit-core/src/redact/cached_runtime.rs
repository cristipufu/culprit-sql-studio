//! Share numerical cache-entry facts without exposing read endpoints, physical owners or private source labels.
use super::{opaque, redact_partition, redact_target};
use crate::{ResetMarker, Result, Snapshot, telemetry::*};
use std::collections::BTreeSet;

pub(super) fn redact(snapshot: &mut Snapshot, mut read: Box<CachedRuntimeRead>) -> Result<Box<CachedRuntimeRead>> {
    let valid = read.is_valid();
    let mut labels = BTreeSet::from([read.description.source.as_str()]);
    if let Some(header) = read.description.observation.snapshot() {
        labels.extend(header.sources.iter().map(|source| source.source.as_str()));
    }
    labels.extend(read.observations.iter().map(|row| row.source.as_str()));
    for source in &mut snapshot.sources {
        if labels.contains(source.source.as_str()) {
            source.source = source_label(&source.source);
        }
    }
    drop(labels);
    if !valid || matches!(read.origin, CachedRuntimeReadOrigin::Captured { .. }) {
        return Err(CollectionStop::InvalidPage.error());
    }
    redact_target(&mut read.description.target);
    redact_partition(&mut read.description.partition);
    read.description.source = source_label(&read.description.source);
    let SourceObservation::Snapshot(header) = &mut read.description.observation else {
        return Err(CollectionStop::InvalidPage.error());
    };
    for source in &mut header.sources {
        source.source = source_label(&source.source);
    }
    for row in &mut read.observations {
        row.source = source_label(&row.source);
        let OperationalFact::MssqlCachedRuntime(runtime) = &mut row.fact else {
            return Err(CollectionStop::InvalidPage.error());
        };
        if let ResetMarker::Generation(generation) = &mut runtime.reset {
            *generation = opaque(generation);
        }
        // Native handles/hashes have a closed hexadecimal representation. Keep their opaque equality, clocks
        // and numerical generations; no statement text, actor label or arbitrary property bag is present.
    }
    read.is_valid().then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
}

fn source_label(value: &str) -> String {
    if value == crate::source_id::PLAN_CACHE { value.into() } else { opaque(value) }
}
