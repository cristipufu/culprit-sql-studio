//! A metadata baseline is verified and frozen before optional native investigations are scheduled.
use super::*;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
#[cfg(test)]
mod tests;
const MAX_BASELINE_ROWS: u32 = 500;
// PREPARE and EXECUTE each consume one native phase: seven owned setup phases, five metadata phases at
// each bound, one consumer read and two phases for the selected-database frame.
const MAX_BASELINE_QUERIES: u32 = 20;
fn bounded_request(request: &CollectionRequest) -> CollectionRequest {
    let mut request = request.clone();
    request.budget.max_snapshot_rows = request.budget.max_snapshot_rows.min(MAX_BASELINE_ROWS);
    request.budget.page_rows = request.budget.page_rows.min(MAX_BASELINE_ROWS);
    request.budget.max_queries = request.budget.max_queries.min(MAX_BASELINE_QUERIES);
    request.budget.statement_timeout_ms = request.budget.statement_timeout_ms.min(1_000);
    request.budget.total_timeout_ms = request.budget.total_timeout_ms.min(5_000);
    request
}
struct ActivityReader<R = ReadSession> {
    io: R,
    connector: Arc<Connector>,
    verified: Option<CollectionDescription>,
}
pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.collection() != TelemetryCollection::Activity {
        return Err(native::invalid());
    }
    crate::probe::database_of(&connector)?;
    let request = bounded_request(request);
    Ok(Box::new(FrozenSource::new(
        ActivityReader {
            io: ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server),
            connector,
            verified: None,
        },
        &request,
    )?))
}
#[async_trait::async_trait]
impl<R: crate::read::OwnedRead> ObservationReader for ActivityReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let capture = capture(&mut self.io, &self.connector, request).await?;
        self.verified = Some(capture.description.clone());
        Ok(capture)
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(description))
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

async fn capture<R: crate::read::QueryRead>(
    reader: &mut R,
    connector: &Connector,
    request: &CollectionRequest,
) -> Result<FrozenObservation> {
    let before = metadata::activity(reader, connector).await?;
    let frame = native::activity::observe(reader, &before, &request.budget).await?;
    let after = metadata::activity(reader, connector).await?;
    if !before.same_activity_source(&after) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let mut coverage = frame.coverage;
    if !after.process_wide {
        coverage.state = culprit_core::SourceState::Partial;
        coverage.complete_scope = false;
        coverage.error_code = Some(ErrorCode::PermissionDenied);
        coverage.stopped.get_or_insert(CollectionStop::SourceUnavailable);
    }
    let description = CollectionDescription {
        target: before.connection.database.clone().into(),
        source: native::activity::SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(digest(
                format!(
                    "{}|{}|{}|{}",
                    before.connection.server_id,
                    before.connection.server_name,
                    before.connection.version,
                    before.started_second.ok_or_else(native::invalid)?
                )
                .as_bytes(),
            )),
            observed_from: canonical_time(&before.connection.at)?,
            observed_until: canonical_time(&after.connection.at)?,
            at: frame.at,
            scope: MeasurementScope::Database,
            population: ActivityPopulation::DefaultDatabaseConnections,
            native_rows: frame.native_rows,
            observations: frame.observations.len() as u32,
            coverage,
        }),
    };
    if !description.observation.activity().is_some_and(ActivitySnapshotDescription::is_valid) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(FrozenObservation { description, records: frame.observations.into_iter().map(TelemetryRecord::from).collect() })
}
