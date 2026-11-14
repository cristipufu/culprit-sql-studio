//! One explicit shared-instance capture, frozen before any warehouse page is delivered.
use super::shared_instance;
use crate::{
    connect::{Connector, Scope},
    native::{self, session_memory as native_memory},
    read::{OwnedRead, ReadSession},
    session_memory, sources,
};
use culprit_core::{Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    shared_instance(request, SnapshotDataset::SessionMemory)?;
    Ok(Box::new(FrozenSource::new(
        Reader {
            io: ReadSession::new(connector, session_memory::bounded_budget(&request.budget), Scope::Server),
            context: None,
        },
        request,
    )?))
}

struct Reader<R: OwnedRead> {
    io: R,
    context: Option<native_memory::Context>,
}

#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let expected = shared_instance(request, SnapshotDataset::SessionMemory)?;
        let limit = native_memory::MAX_ROWS.min(request.budget.max_snapshot_rows as usize);
        let captured = session_memory::capture(&mut self.io, limit, Some(expected)).await?;
        let source_coverage = captured.source_coverage();
        let snapshot = captured.snapshot;
        let description = CollectionDescription {
            target: CollectionTarget::Shared(snapshot.resource),
            source: "mysql-session-memory".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::SessionMemory,
                observed_from: snapshot.observed_from,
                observed_until: snapshot.observed_until,
                records: snapshot.observations.len() as u32,
                sources: vec![source_coverage],
            }),
        };
        let records = snapshot
            .observations
            .into_iter()
            .map(|observation| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: sources::SESSION_MEMORY.name.into(),
                    key: observation.key()?,
                    fact: OperationalFact::SessionMemory(Box::new(observation)),
                }))
            })
            .collect::<Result<_>>()?;
        self.context = Some(captured.context);
        Ok(FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let (after, _) = native_memory::context(&mut self.io).await?;
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
