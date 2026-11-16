//! Provisional scan projection of the same bounded SHOW rows used by retained collection.
use crate::{connect::Flavor, native, read::QueryRead, sources};
use culprit_core::{Coverage, Error, Result, Snapshot, telemetry::*};
use std::time::SystemTime;

pub(crate) struct Reading {
    pub snapshot: ReplicationSnapshot,
    pub error: Option<Error>,
}

pub(super) async fn read(reader: &mut impl QueryRead, flavor: Flavor, version: &str) -> Result<Reading> {
    let observed_from = SystemTime::now();
    let result = native::replication::read(reader, flavor, version).await;
    let context = ObservationReadContext::collector(observed_from, SystemTime::now())?;
    let (observations, coverage, error) = match result {
        Ok(observations) => {
            let coverage =
                StateSourceCoverage::collected(native::replication::SOURCE.into(), observations.len() as u32);
            (observations, coverage, None)
        }
        Err(error) => {
            let stopped = match error.detail {
                Some(culprit_core::ErrorDetail::CollectionStopped(stop)) => Some(stop),
                _ => Some(CollectionStop::SourceUnavailable),
            };
            let coverage = StateSourceCoverage::unavailable(native::replication::SOURCE.into(), error.code, stopped);
            (vec![], coverage, Some(error))
        }
    };
    let snapshot = ReplicationSnapshot { context, sources: vec![coverage], observations };
    if !snapshot.is_valid() {
        return Err(native::invalid());
    }
    Ok(Reading { snapshot, error })
}

/// The outer scan owns the shared native reader. A successful SHOW response remains provisional until it
/// confirms shutdown; do not publish positive points, empty coverage or legacy grades on uncertain completion.
pub(crate) fn settle(snapshot: &mut Snapshot, reading: Result<Reading>, shutdown: SourceShutdown) {
    let mut coverage = Coverage::default();
    match reading {
        Ok(reading) => publish(snapshot, reading, shutdown, &mut coverage),
        Err(error) => coverage.source_failed(&sources::REPLICATION, &error),
    }
    if shutdown == SourceShutdown::Unconfirmed {
        coverage.source_failed(&sources::REPLICATION, &CollectionStop::CancellationUnconfirmed.error());
    }
    coverage.append_to(snapshot);
}

fn publish(snapshot: &mut Snapshot, mut reading: Reading, shutdown: SourceShutdown, coverage: &mut Coverage) {
    if let Some(error) = &reading.error {
        coverage.source_failed(&sources::REPLICATION, error);
    }
    if shutdown == SourceShutdown::Unconfirmed {
        reading.snapshot.observations.clear();
        if reading.error.is_none() {
            let error = CollectionStop::CancellationUnconfirmed.error();
            reading.snapshot.sources = vec![StateSourceCoverage::unavailable(
                native::replication::SOURCE.into(),
                error.code,
                Some(CollectionStop::CancellationUnconfirmed),
            )];
        }
    } else if reading.error.is_none() {
        coverage.completed(&sources::REPLICATION, reading.snapshot.observations.len());
    }
    let pressure = snapshot.pressure.get_or_insert_default();
    pressure.replicas = reading
        .snapshot
        .observations
        .iter()
        .filter_map(|point| {
            let ReplicationFact::Native(native) = &point.fact else { return None };
            super::replica(super::ChannelRow::from(native.as_ref()))
        })
        .collect();
    pressure.replication = Some(Box::new(reading.snapshot));
}

#[cfg(test)]
mod tests;
