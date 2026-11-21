//! The baseline persists native metadata before any optional SQL or owner investigation.
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    conn::Pool,
    query_store::decode,
};
use async_trait::async_trait;
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::sync::Arc;

const MAX_BASELINE_ROWS: u32 = 500;
const MAX_BASELINE_READER_QUERIES: u32 = 7;
const MAX_BASELINE_QUERIES: u32 = MAX_BASELINE_READER_QUERIES + crate::read::ReadSession::SESSION_SETUP_QUERIES;

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
        let options = ReadOptions { budget: request.budget.clone(), identity: IdentityDetail::Endpoint };
        let reader = NativeActivityReader {
            reader: ActivityReader::open_collection(pool, options, host, port)?,
            verified: None,
        };
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
    let pending = crate::activity::capture::PendingActivityFrame::begin_census(reader).await?;
    if !matches!(pending.baseline.frame.context.edition, 2 | 3 | 4 | 5 | 8) || pending.baseline.frame.context.major < 11
    {
        return Err(culprit_core::Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let completed = pending.finish(reader).await?;
    Ok((completed.description, export_observations(completed.frame.records)))
}
pub(crate) fn export_observations(records: Vec<crate::activity::ActivityRecord>) -> Vec<ActivityObservation> {
    let mut observations: Vec<_> =
        records.into_iter().map(crate::activity::ActivityRecord::into_redacted_observation).collect();
    observations.sort_by(|a, b| a.key.cmp(&b.key));
    observations
}

#[cfg(test)]
mod tests;
