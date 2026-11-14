//! The baseline persists native metadata before any optional SQL or owner investigation.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions, database_context::DatabaseContext},
    conn::Pool,
    query_store::decode,
};
use async_trait::async_trait;
use culprit_core::{ErrorCode, MeasurementScope, Result, telemetry::*};
use std::sync::Arc;

const MAX_BASELINE_ROWS: u32 = 500;
const MAX_BASELINE_QUERIES: u32 = 7;

pub(super) fn bounded_budget(budget: &CollectionBudget) -> CollectionBudget {
    let mut bounded = budget.clone();
    bounded.max_snapshot_rows = bounded.max_snapshot_rows.min(MAX_BASELINE_ROWS);
    bounded.page_rows = bounded.page_rows.min(MAX_BASELINE_ROWS);
    bounded.max_queries = bounded.max_queries.min(MAX_BASELINE_QUERIES);
    bounded.statement_timeout_ms = bounded.statement_timeout_ms.min(1_000);
    bounded.total_timeout_ms = bounded.total_timeout_ms.min(5_000);
    bounded
}

pub(super) struct ActivitySource {
    inner: culprit_read::FrozenSource<NativeActivityReader>,
}
struct NativeActivityReader {
    reader: ActivityReader,
    verified: Option<CollectionDescription>,
}
impl ActivitySource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        if request.collection() != TelemetryCollection::Activity {
            return Err(decode::malformed());
        }
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        };
        let reader = NativeActivityReader { reader: ActivityReader::open(pool, options, host, port)?, verified: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}
#[async_trait]
impl culprit_read::ObservationReader for NativeActivityReader {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let (description, observations) = capture(&mut self.reader).await?;
        self.verified = Some(description.clone());
        Ok(culprit_read::FrozenObservation {
            description,
            records: observations.into_iter().map(TelemetryRecord::from).collect(),
        })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(description))
    }
    fn query_count(&self) -> u32 {
        self.reader.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}
#[async_trait]
impl TelemetrySource for ActivitySource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.inner.describe().await
    }
    fn query_count(&self) -> u32 {
        self.inner.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.inner.read_page(stream, cursor).await
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.inner.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}

pub(crate) async fn capture<R: crate::read::NativeRead>(
    reader: &mut ActivityReader<R>,
) -> Result<(CollectionDescription, Vec<ActivityObservation>)> {
    let before = AuthorityRead::admit(DatabaseContext::initial(reader).await)?;
    if let Some(context) = &before.context {
        crate::read::require_primary(context.activity.writable)?;
    }
    let mut baseline = reader.observe_baseline().await?;
    crate::read::require_primary(baseline.frame.context.writable)?;
    let after = AuthorityRead::admit(match &before.context {
        Some(context) => DatabaseContext::recheck(reader, context).await,
        None => DatabaseContext::initial(reader).await,
    })?;
    for context in [&before.context, &after.context].into_iter().flatten() {
        if !context.matches_frame(&baseline.frame.context) {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    let qualified = match (&before.context, &after.context) {
        (Some(before), Some(after)) if before.is_qualified() && after.is_qualified() => {
            if !before.same_source(after) {
                return Err(CollectionStop::SourceChanged.error());
            }
            true
        }
        _ => false,
    };
    let database_wide = [&before.context, &after.context].into_iter().flatten().all(|context| context.database_wide);
    if !database_wide || !qualified {
        baseline.coverage.state = culprit_core::SourceState::Partial;
        baseline.coverage.complete_scope = false;
        baseline.coverage.error_code = before.error_code.or(after.error_code).or(baseline.coverage.error_code);
        if !database_wide {
            baseline.coverage.error_code = Some(ErrorCode::PermissionDenied);
        }
        baseline.coverage.stopped.get_or_insert(CollectionStop::SourceUnavailable);
    }
    let observed_from = before
        .context
        .as_ref()
        .map_or_else(|| baseline.frame.context.at.clone(), |context| context.activity.at.clone());
    let observed_until =
        after.context.as_ref().map_or_else(|| baseline.frame.context.at.clone(), |context| context.activity.at.clone());
    let mut context = match &before.context {
        Some(before) => before.capture_context()?,
        None => baseline.frame.context.clone(),
    };
    if !qualified {
        context.epoch = None;
    }
    let at = baseline.frame.context.at;
    let observations = export_observations(baseline.frame.records);
    let activity = ActivitySnapshotDescription {
        epoch: context.epoch,
        observed_from,
        observed_until,
        at,
        scope: MeasurementScope::Database,
        population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
        native_rows: baseline.native_rows,
        observations: observations.len() as u32,
        coverage: baseline.coverage,
    };
    if !activity.is_valid() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let description = CollectionDescription {
        target: context.database.into(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(activity),
    };
    Ok((description, observations))
}
/// A completed metadata authority failure cannot erase the independent native actor frame. Terminal
/// ownership/budget failures still stop all follow-up work and require the enclosing source to close.
struct AuthorityRead {
    context: Option<DatabaseContext>,
    error_code: Option<ErrorCode>,
}
impl AuthorityRead {
    fn admit(result: Result<DatabaseContext>) -> Result<Self> {
        match result {
            Ok(context) => Ok(Self { error_code: context.qualification_error, context: Some(context) }),
            Err(error) if crate::read::stops_remaining_reads(&error) => Err(error),
            Err(error) => Ok(Self { context: None, error_code: Some(error.code) }),
        }
    }
}
pub(crate) fn export_observations(records: Vec<crate::activity::ActivityRecord>) -> Vec<ActivityObservation> {
    let mut observations: Vec<_> =
        records.into_iter().map(crate::activity::ActivityRecord::into_redacted_observation).collect();
    observations.sort_by(|a, b| a.key.cmp(&b.key));
    observations
}

#[cfg(test)]
mod tests;
