//! Explicit current page targets: native acquisition is identical to scans, while FrozenSource owns delivery.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions, page_targets::PageWaitGuard},
    conn::Pool,
    read::{NativeRead, ReadSession, invalid_data},
    sources,
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) struct PageWaitSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<PageWaitReader<R>>,
}
struct PageWaitReader<R: NativeRead> {
    reader: ActivityReader<R>,
    guard: Option<PageWaitGuard>,
}
impl PageWaitSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let request = bounded_request(request)?;
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        };
        Self::from_reader(ActivityReader::open(pool, options, host, port)?, &request)
    }
}
fn bounded_request(request: &CollectionRequest) -> Result<CollectionRequest> {
    request.validate()?;
    let mut bounded = request.clone();
    bounded.budget = if matches!(request.selection, CollectionSelection::FocusedPageWait { .. }) {
        focused_budget(&request.budget)
    } else {
        crate::page_waits::bounded_budget(&request.budget)
    };
    bounded.budget.validate()?;
    Ok(bounded)
}
pub(super) fn focused_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = crate::page_waits::bounded_budget(requested);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(1);
    budget.page_rows = budget.page_rows.min(1);
    budget
}
impl<R: NativeRead> PageWaitSource<R> {
    pub(super) fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.subject.require_database()?;
        if !matches!(
            request.selection,
            CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }
                | CollectionSelection::FocusedPageWait { .. }
        ) {
            return Err(invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(PageWaitReader { reader, guard: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for PageWaitReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let (captured, result) = match &request.selection {
            CollectionSelection::FocusedPageWait { focus } => {
                let focused = self.reader.capture_focused_page_wait_targets(focus).await?;
                (focused.capture, Some(focused.result))
            }
            _ => (self.reader.capture_page_wait_targets().await?, None),
        };
        let records = captured
            .snapshot
            .observations
            .into_iter()
            .map(|row| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: sources::PAGE_WAIT_TARGETS.name.into(),
                    key: row.key()?,
                    fact: OperationalFact::PageWait(Box::new(row)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut coverage =
            StateSourceCoverage::observed(sources::PAGE_WAIT_TARGETS.name.into(), records.len() as u32, false);
        coverage.stopped = captured.limited.then_some(CollectionStop::RowLimit);
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::PageWaitTargets,
            observed_from: captured.snapshot.observed_from,
            observed_until: captured.snapshot.observed_until,
            records: records.len() as u32,
            // A verified frozen row population is still only native request/coordinator coverage.
            sources: vec![coverage],
        };
        let observation = match result {
            Some(result) => SourceObservation::FocusedPageWait { snapshot, result: Box::new(result) },
            None => SourceObservation::Snapshot(snapshot),
        };
        let description = CollectionDescription {
            target: captured.context.database.into(),
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation,
        };
        self.guard = Some(captured.guard);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        self.reader.verify_page_wait_context(self.guard.as_ref().ok_or_else(invalid_data)?).await
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
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for PageWaitSource<R> {
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

#[cfg(test)]
mod tests;
