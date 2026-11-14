//! An unfiltered metadata baseline. Optional blocker/SQL reads belong to a later investigation.
use super::*;
use crate::native::activity;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
#[cfg(test)]
mod tests;

const MAX_BASELINE_ROWS: u32 = 500;
const MAX_BASELINE_QUERIES: u32 = 4; // Owned session setup, before/after metadata and one activity projection.
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(MAX_BASELINE_ROWS);
    budget.page_rows = budget.page_rows.min(MAX_BASELINE_ROWS);
    budget.max_queries = budget.max_queries.min(MAX_BASELINE_QUERIES);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(1_000);
    budget.total_timeout_ms = budget.total_timeout_ms.min(5_000);
    budget
}
struct ActivityReader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.collection() != TelemetryCollection::Activity {
        return Err(native::invalid());
    }
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    let reader = ActivityReader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        verified: None,
    };
    Ok(Box::new(FrozenSource::new(reader, &request)?))
}
#[async_trait::async_trait]
impl<R: crate::read::NativeRead> ObservationReader for ActivityReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let capture = capture(&mut self.io, &self.host, self.port, request).await?;
        self.verified = Some(capture.description.clone());
        Ok(capture)
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        // Required source checks already bracket the frozen native observation. Paging performs no more SQL.
        Ok(self.verified.as_ref() == Some(description))
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

async fn capture<R: crate::read::QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &CollectionRequest,
) -> Result<FrozenObservation> {
    let before = metadata::activity(reader, host, port).await?;
    if before.version < 120_000 {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let read = activity::observe_baseline_rows(
        reader,
        &activity::query(before.version, request.budget.max_snapshot_rows as usize + 1),
        &before.database_oid,
        &request.budget,
    )
    .await?;
    let after = metadata::activity(reader, host, port).await?;
    if !before.same_activity_source(&after) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let visible = before.full_visibility && after.full_visibility && read.frame.fully_visible_for(&before.database_oid);
    let mut observations = read.frame.observations();
    for row in &mut observations {
        row.blocking_status = ActivityBlockingStatus::Unknown;
    }
    let source = "pg_stat_activity";
    let mut coverage =
        StateSourceCoverage::observed(source.into(), observations.len() as u32, visible && !read.limited);
    if !before.full_visibility || !after.full_visibility {
        coverage.error_code = Some(ErrorCode::PermissionDenied);
    }
    if read.limited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    } else if !visible {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let description = CollectionDescription {
        target: before.database.clone().into(),
        source: source.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(digest(
                format!("{}|{}|{}|pg-activity-v1", before.database_oid, before.started_at, before.version).as_bytes(),
            )),
            observed_from: canonical_time(&before.at)?,
            observed_until: canonical_time(&after.at)?,
            at: read.frame.at,
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: read.native_rows,
            observations: observations.len() as u32,
            coverage,
        }),
    };
    if !description.observation.activity().is_some_and(ActivitySnapshotDescription::is_valid) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(FrozenObservation { description, records: observations.into_iter().map(TelemetryRecord::from).collect() })
}
