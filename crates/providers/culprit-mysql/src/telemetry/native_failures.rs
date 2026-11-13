//! The explicit shared-instance route freezes one bounded retained buffer before delivering pages.
use super::shared_instance;
use crate::{
    connect::{Connector, Scope},
    native::{self, error_log, error_log::context},
    native_failures,
    read::{OwnedRead, ReadSession},
    sources,
};
use culprit_core::{Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    shared_instance(request, SnapshotDataset::NativeFailures)?;
    Ok(Box::new(FrozenSource::new(
        Reader { io: ReadSession::new(connector, request.budget.clone(), Scope::Server), context: None },
        request,
    )?))
}

struct Reader<R: OwnedRead> {
    io: R,
    context: Option<context::Context>,
}

#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let resource = shared_instance(request, SnapshotDataset::NativeFailures)?;
        let limit = error_log::MAX_ROWS.min(request.budget.max_snapshot_rows as usize);
        let captured = native_failures::capture(&mut self.io, limit, Some(resource)).await?;
        let source_name = sources::NATIVE_FAILURES.name;
        let description = CollectionDescription {
            target: CollectionTarget::Shared(captured.context.instance.resource.clone()),
            source: "mysql-error-log".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::NativeFailures,
                observed_from: captured.observed_from,
                observed_until: captured.observed_until,
                records: captured.observations.rows.len() as u32,
                sources: vec![StateSourceCoverage::observed(
                    source_name.into(),
                    captured.observations.rows.len() as u32,
                    !captured.observations.truncated,
                )],
            }),
        };
        let records = captured
            .observations
            .rows
            .into_iter()
            .map(|observation| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: source_name.into(),
                    key: observation.key()?,
                    fact: OperationalFact::MysqlErrorLog(Box::new(observation)),
                }))
            })
            .collect::<Result<_>>()?;
        self.context = Some(captured.context);
        Ok(FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let (after, _) = context::read(&mut self.io).await?;
        Ok(before == &after)
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

#[cfg(test)]
mod tests;
