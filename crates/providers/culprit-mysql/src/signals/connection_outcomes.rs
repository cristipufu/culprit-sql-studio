//! Connection outcomes reuse one native status read and remain provisional until the shared reader settles.
use crate::{native, read::QueryRead, sources};
use culprit_core::{
    ConnectionOutcomeObservation, ConnectionOutcomeSnapshot, Coverage, Error, Result, Snapshot, telemetry::*,
};
use std::time::SystemTime;

pub(crate) struct Reading {
    snapshot: Box<ConnectionOutcomeSnapshot>,
    error: Option<Error>,
}

pub(super) async fn read(reader: &mut impl QueryRead, target: CollectionTarget) -> Result<Reading> {
    let from = SystemTime::now();
    let result = native::operational::connections::read(reader).await;
    let context = ObservationReadContext::collector(from, SystemTime::now())?;
    let (observations, source, error) = match result {
        Ok(rows) => {
            let observations =
                rows.iter().map(ConnectionOutcomeObservation::from_operational).collect::<Result<Vec<_>>>()?;
            // This scan retains a selected variable family, not the full global-status source inventory.
            let source = StateSourceCoverage::observed(
                native::operational::Kind::Status.source().into(),
                observations.len() as u32,
                false,
            );
            (observations, source, None)
        }
        Err(error) => {
            let stopped = match error.detail {
                Some(culprit_core::ErrorDetail::CollectionStopped(stop)) => Some(stop),
                _ => None,
            };
            let source = StateSourceCoverage::unavailable(
                native::operational::Kind::Status.source().into(),
                error.code,
                stopped,
            );
            (vec![], source, Some(error))
        }
    };
    let snapshot = Box::new(ConnectionOutcomeSnapshot { target, context, sources: vec![source], observations });
    if !snapshot.is_valid() {
        return Err(native::invalid());
    }
    Ok(Reading { snapshot, error })
}

pub(crate) fn settle(snapshot: &mut Snapshot, reading: Result<Reading>, shutdown: SourceShutdown) {
    let mut coverage = Coverage::default();
    // This replaces one complete acquisition, rather than adding an independently read source part. Previous
    // notes remain history, but its current outcome must describe the same read as the replacement carrier.
    snapshot.sources.retain(|source| !culprit_core::same_source(&source.source, sources::CONNECTIONS.name));
    snapshot.pressure.get_or_insert_default().connection_outcomes = None;
    match reading {
        Ok(mut reading) => {
            if let Some(error) = &reading.error {
                coverage.source_failed(&sources::CONNECTIONS, error);
            }
            if shutdown == SourceShutdown::Unconfirmed {
                let error = CollectionStop::CancellationUnconfirmed.error();
                reading.snapshot.observations.clear();
                // A prior native source failure keeps its own code. No provisional positive point survives.
                if reading.error.is_none() {
                    reading.snapshot.sources = vec![StateSourceCoverage::unavailable(
                        native::operational::Kind::Status.source().into(),
                        error.code,
                        Some(CollectionStop::CancellationUnconfirmed),
                    )];
                }
            } else if reading.error.is_none() {
                coverage.completed(&sources::CONNECTIONS, reading.snapshot.observations.len());
            }
            snapshot.pressure.get_or_insert_default().connection_outcomes = Some(reading.snapshot);
        }
        Err(error) => {
            coverage.source_failed(&sources::CONNECTIONS, &error);
        }
    }
    if shutdown == SourceShutdown::Unconfirmed {
        coverage.source_failed(&sources::CONNECTIONS, &CollectionStop::CancellationUnconfirmed.error());
    }
    coverage.append_to(snapshot);
}

#[cfg(test)]
mod tests;
