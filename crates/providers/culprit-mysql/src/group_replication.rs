//! An explicit bounded capture. The same native owner is reusable by a buffered consumer; scans are not widened.
use crate::connect::Connector;
use crate::{
    connect::Scope,
    native::{self, group_replication as group},
    read::{OwnedRead, ReadSession},
};
use culprit_core::{Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

// Seven existing setup statements, three four-query contexts, and two PREPARE/EXECUTE data pairs.
const MAX_QUERIES: u32 = 23;
pub(crate) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: (MYSQL_GROUP_MAX_MEMBERS * 2) as u32,
        page_rows: 64,
        page_bytes: 256 * 1024,
        total_bytes: 1024 * 1024,
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
    super::telemetry::require_group_instance(request)?;
    Ok(Box::new(FrozenSource::new(
        Reader { io: ReadSession::new(connector, bounded_budget(&request.budget), Scope::Server), context: None },
        request,
    )?))
}
struct Reader<R: OwnedRead> {
    io: R,
    context: Option<group::Context>,
}
#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let expected = super::telemetry::require_group_instance(request)?;
        let (context, from) = group::context(&mut self.io).await?;
        if expected != &context.instance.resource {
            return Err(CollectionStop::SourceChanged.error());
        }
        let limit = MYSQL_GROUP_MAX_MEMBERS.min(request.budget.max_snapshot_rows as usize / 2);
        let observed = group::read(&mut self.io, &context, limit).await?;
        let (after, until) = group::context(&mut self.io).await?;
        if !context.same_source(&after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let description = CollectionDescription {
            target: CollectionTarget::Shared(context.instance.resource.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(context.instance.resource.clone()),
                epoch: Some(native::local_context::process_epoch(context.instance.started_second)),
            },
            source: MYSQL_GROUP_REPLICATION_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::GroupReplication {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::GroupReplication,
                    observed_from: from.clone(),
                    observed_until: until,
                    records: observed.rows.len() as u32,
                    sources: observed.coverage,
                },
                metadata: Box::new(context.metadata.clone()),
            },
        };
        group_replication_context(&description)?;
        if observed.rows.iter().any(|row| !matches!(&row.fact, OperationalFact::MysqlGroupReplication(point) if point.fits_source(&description))) {
            return Err(native::invalid());
        }
        self.context = Some(context);
        Ok(FrozenObservation {
            description,
            records: observed.rows.into_iter().map(TelemetryRecord::Operational).collect(),
        })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let (after, at) = group::context(&mut self.io).await?;
        let snapshot = description.observation.snapshot().ok_or_else(native::invalid)?;
        Ok(before.same_source(&after) && ObservationBounds::new(&snapshot.observed_until, &at).is_some())
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
