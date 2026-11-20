//! One optional event catalog acquisition feeds scans and the existing retained Jobs owner.
use crate::{
    connect::{Connector, Scope},
    native::{self, scheduled_events as events},
    read::{OwnedRead, ReadSession, confirmed_result},
    sources,
};
use culprit_core::{Coverage, Result, Snapshot, SourceState, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

// At most seven native owner setup statements, three five-dispatch native contexts
// (two prepared probes and one process-clock read each), layout1, prepared event projection2.
const MAX_QUERIES: u32 = 7 + 3 * 5 + 1 + 2;
pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    requested.intersection(&CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: MAX_MYSQL_SCHEDULED_EVENTS as u32,
        page_rows: 32,
        page_bytes: 64 * 1024,
        total_bytes: 512 * 1024,
        statement_timeout_ms: 2000,
        total_timeout_ms: 10000,
        ..CollectionBudget::upper_limits()
    })
}
fn description(snapshot: &MysqlEventsSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: snapshot.context.target.clone(),
        partition: snapshot.context.partition.clone(),
        source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MysqlEvents {
            metadata: Box::new(snapshot.metadata.clone()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: snapshot.context.observed_from.clone(),
                observed_until: snapshot.context.observed_until.clone(),
                records: snapshot.observations.len() as u32,
                sources: snapshot.sources.clone(),
            },
        },
    }
}

pub(crate) async fn collect(connector: Arc<Connector>, snapshot: &mut Snapshot) {
    let budget = bounded_budget(&CollectionBudget::default());
    let mut reader = ReadSession::new(connector.clone(), budget.clone(), Scope::Server);
    let result = events::capture(&mut reader, &connector, &budget).await;
    let result = confirmed_result(&mut reader, result).await;
    let mut coverage = Coverage::default();
    match result {
        Ok(captured) => {
            let state = &captured.snapshot.sources[0];
            if matches!(state.state, SourceState::Collected | SourceState::Empty) {
                coverage.recorded(&sources::SCHEDULED_EVENTS, state.state, None);
            } else {
                coverage.recorded_message(
                    &sources::SCHEDULED_EVENTS,
                    state.state,
                    culprit_core::message!("provider.mysql.scheduled_events.coverage"),
                );
            }
            snapshot.operations.jobs = Some(Box::new(JobsSnapshot::MysqlEvents {
                snapshot: Box::new(captured.snapshot),
                read_complete: true,
            }));
        }
        Err(error) => coverage.source_failed(&sources::SCHEDULED_EVENTS, &error),
    }
    coverage.append_to(snapshot);
}
struct Reader<R = ReadSession> {
    io: R,
    connector: Arc<Connector>,
    context: Option<events::Context>,
}
pub(crate) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs }) {
        return Err(native::invalid());
    }
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    request.budget.validate()?;
    let io = ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server);
    Ok(Box::new(FrozenSource::new(Reader { io, connector, context: None }, &request)?))
}
#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let captured = events::capture(&mut self.io, &self.connector, &request.budget).await?;
        let description = description(&captured.snapshot);
        if !description.matches_request(request) {
            return Err(native::invalid());
        }
        self.context = Some(captured.context);
        Ok(FrozenObservation {
            description,
            records: captured.snapshot.observations.into_iter().map(TelemetryRecord::Operational).collect(),
        })
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let after = events::Context::read(&mut self.io, &self.connector).await?;
        Ok(before.verifies(&after, source))
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
