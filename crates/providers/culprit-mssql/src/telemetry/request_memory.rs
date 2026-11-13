//! Frozen paging and capture ownership stay generic; native grant/request reading is shared with explicit scans.
use crate::{
    activity::{ActivityContext, ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
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
    context: Option<ActivityContext>,
}
impl MemorySource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        };
        Self::from_reader(ActivityReader::open(pool, options, host, port)?, request)
    }
}
impl<R: NativeRead> MemorySource<R> {
    fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory }) {
            return Err(crate::read::invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(MemoryObservationReader { reader, context: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for MemoryObservationReader<R> {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let captured = self.reader.capture_memory().await?;
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
        let description = CollectionDescription {
            target: captured.context.database.clone().into(),
            source: sources::CURRENT_REQUEST_MEMORY.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestMemory,
                observed_from: at.clone(),
                observed_until: at,
                records: records.len() as u32,
                sources: vec![StateSourceCoverage::observed(
                    sources::CURRENT_REQUEST_MEMORY.name.into(),
                    records.len() as u32,
                    captured.complete_scope,
                )],
            }),
        };
        self.context = Some(captured.context);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        self.reader.verify_memory_context(self.context.as_ref().ok_or_else(crate::read::invalid_data)?).await
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
