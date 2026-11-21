//! Explicit Galera capture over the same owned MySQL/MariaDB native session and immutable snapshot protocol.
use crate::{
    connect::{Connector, Scope},
    native::{self, galera},
    read::{OwnedRead, ReadSession},
};
use culprit_core::{ErrorCode, Result, SourceState, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

// Existing seven setup statements + three native three-query contexts + one fixed SHOW status query.
const MAX_QUERIES: u32 = 17;
pub(crate) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: 1,
        page_rows: 1,
        page_bytes: 32 * 1024,
        total_bytes: 32 * 1024,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
        ..Default::default()
    }
}
pub(crate) fn bounded_budget(request: &CollectionBudget) -> CollectionBudget {
    request.intersection(&budget())
}
pub(crate) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    super::telemetry::require_galera_instance(request)?;
    Ok(Box::new(FrozenSource::new(
        Reader {
            io: ReadSession::new(connector, bounded_budget(&request.budget), Scope::Server),
            context: None,
            until: None,
        },
        request,
    )?))
}
struct Reader<R: OwnedRead> {
    io: R,
    context: Option<galera::Context>,
    until: Option<String>,
}
#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let expected = super::telemetry::require_galera_instance(request)?;
        let context = galera::context(&mut self.io).await?;
        if expected != &context.instance.resource {
            return Err(CollectionStop::SourceChanged.error());
        }
        let status = if context.metadata.enabled == Some(true) {
            Some(match galera::capture_status(&mut self.io).await {
                Ok(status) => Ok(status),
                Err(error) if error.code == ErrorCode::PermissionDenied && error.detail.is_none() => Err(error),
                Err(error) => return Err(error),
            })
        } else {
            None
        };
        let after = galera::context(&mut self.io).await?;
        let from = context.metadata.observed_at.clone();
        let until = after.metadata.observed_at.clone();
        if !context.same_source(&after) || ObservationBounds::new(&from, &until).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let (records, coverage) = match status {
            None => (
                Vec::new(),
                StateSourceCoverage {
                    source: MARIADB_GALERA_SOURCE.into(),
                    state: if context.metadata.enabled == Some(false) {
                        SourceState::NotApplicable
                    } else {
                        SourceState::Unavailable
                    },
                    records: 0,
                    complete_scope: false,
                    error_code: if context.metadata.enabled.is_none() { Some(ErrorCode::Unsupported) } else { None },
                    stopped: None,
                },
            ),
            Some(Err(error)) if error.code == ErrorCode::PermissionDenied && error.detail.is_none() => {
                (Vec::new(), unavailable(error.code))
            }
            Some(Err(error)) => return Err(error),
            Some(Ok(status)) if status.is_empty() => (Vec::new(), unavailable(ErrorCode::Unsupported)),
            Some(Ok(status)) => {
                let point = status.point(&from, &until)?;
                let row = OperationalObservation {
                    source: MARIADB_GALERA_SOURCE.into(),
                    key: point.key()?,
                    fact: OperationalFact::MariadbGalera(Box::new(point)),
                };
                (
                    vec![TelemetryRecord::Operational(row)],
                    StateSourceCoverage::observed(MARIADB_GALERA_SOURCE.into(), 1, status.complete()),
                )
            }
        };
        let description = CollectionDescription {
            target: CollectionTarget::Shared(context.instance.resource.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(context.instance.resource.clone()),
                epoch: Some(native::local_context::process_epoch(context.instance.started_second)),
            },
            source: MARIADB_GALERA_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Galera {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::Galera,
                    observed_from: from,
                    observed_until: until.clone(),
                    records: records.len() as u32,
                    sources: vec![coverage],
                },
                metadata: Box::new(context.metadata.clone()),
            },
        };
        galera_context(&description)?;
        if records.iter().any(|row| !matches!(row, TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MariadbGalera(point), .. }) if point.fits_source(&description))) {
            return Err(native::invalid());
        }
        self.context = Some(context);
        self.until = Some(until);
        Ok(FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let after = galera::context(&mut self.io).await?;
        Ok(before.same_source(&after)
            && self
                .until
                .as_ref()
                .is_some_and(|until| ObservationBounds::new(until, &after.metadata.observed_at).is_some()))
    }
    fn query_count(&self) -> u32 {
        self.io.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
fn unavailable(error: ErrorCode) -> StateSourceCoverage {
    StateSourceCoverage {
        source: MARIADB_GALERA_SOURCE.into(),
        state: SourceState::Unavailable,
        records: 0,
        complete_scope: false,
        error_code: Some(error),
        stopped: None,
    }
}
#[cfg(test)]
mod tests;
