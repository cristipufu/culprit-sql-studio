//! Frozen paging and capture ownership stay generic; native grant/request reading is shared with explicit scans.
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    conn::Pool,
    read::{NativeRead, ReadSession},
    sources,
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) struct MemorySource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<MemoryObservationReader<R>>,
}
struct MemoryObservationReader<R: NativeRead> {
    reader: ActivityReader<R>,
    context: Option<crate::local_source::Context>,
}
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    ReadSession::budget_including_setup(requested, crate::request_memory::bounded_budget(requested))
}
impl MemorySource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::from_reader(ActivityReader::open_collection(pool, read_options(request), host, port)?, request)
    }
}
fn read_options(request: &CollectionRequest) -> ReadOptions {
    ReadOptions { budget: bounded_budget(&request.budget), identity: IdentityDetail::Endpoint }
}
impl<R: NativeRead> MemorySource<R> {
    pub(super) fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.subject.require_database()?;
        if !matches!(
            request.selection,
            CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory }
                | CollectionSelection::FocusedRequestMemory { .. }
        ) {
            return Err(crate::read::invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(MemoryObservationReader { reader, context: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for MemoryObservationReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let (captured, focused) = match &request.selection {
            CollectionSelection::FocusedRequestMemory { focus } => {
                let observed = self.reader.capture_focused_memory(focus).await?;
                (observed.capture, Some(observed.result))
            }
            _ => (self.reader.capture_memory().await?, None),
        };
        let at = captured.context.at.clone();
        let records = captured
            .observations
            .into_iter()
            .map(|grant| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: sources::CURRENT_REQUEST_MEMORY.name.into(),
                    key: grant.key()?,
                    fact: OperationalFact::ExecutionGrant(Box::new(grant)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::CurrentRequestMemory,
            observed_from: focused.as_ref().map_or_else(|| at.clone(), |result| result.observed_from.clone()),
            observed_until: at,
            records: records.len() as u32,
            sources: vec![StateSourceCoverage::observed(
                sources::CURRENT_REQUEST_MEMORY.name.into(),
                records.len() as u32,
                captured.complete_scope,
            )],
        };
        let description = CollectionDescription {
            partition: captured.owner.source_partition()?,
            target: captured.owner.activity.database.clone().into(),
            source: sources::CURRENT_REQUEST_MEMORY.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: match focused {
                Some(result) => SourceObservation::FocusedRequestMemory { snapshot, result: Box::new(result) },
                None => SourceObservation::Snapshot(snapshot),
            },
        };
        self.context = Some(captured.owner);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        match self.reader.verify_memory_context(self.context.as_ref().ok_or_else(crate::read::invalid_data)?).await {
            Ok(_) => Ok(true),
            Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => Ok(false),
            Err(error) => Err(error),
        }
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
impl<R: NativeRead> TelemetrySource for MemorySource<R> {
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

#[cfg(test)]
mod focused_tests;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod profile_tests;
