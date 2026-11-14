//! Snapshot paging, cursors and close proof remain in FrozenSource. Native task and request reading is shared.
use crate::{
    activity::{ActivityContext, ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::Pool,
    read::{NativeRead, ReadSession, invalid_data},
    sources,
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) struct TaskSpaceSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<TaskSpaceReader<R>>,
}
struct TaskSpaceReader<R: NativeRead> {
    reader: ActivityReader<R>,
    context: Option<ActivityContext>,
}

impl TaskSpaceSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let options = ReadOptions {
            budget: crate::task_space::bounded_budget(&request.budget),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        };
        Self::from_reader(ActivityReader::open(pool, options, host, port)?, request)
    }
}
impl<R: NativeRead> TaskSpaceSource<R> {
    fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::TaskSpace }) {
            return Err(invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(TaskSpaceReader { reader, context: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for TaskSpaceReader<R> {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let captured = self.reader.capture_task_space().await?;
        let snapshot = captured.snapshot;
        let complete = !captured.limited && captured.context_complete;
        let records = snapshot
            .observations
            .into_iter()
            .map(|row| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: sources::TASK_SPACE.name.into(),
                    key: row.key()?,
                    fact: OperationalFact::TaskSpace(Box::new(row)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut coverage =
            StateSourceCoverage::observed(sources::TASK_SPACE.name.into(), records.len() as u32, complete);
        coverage.error_code = captured.context_error_code;
        let description = CollectionDescription {
            target: captured.context.database.clone().into(),
            source: sources::TASK_SPACE.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::TaskSpace,
                observed_from: snapshot.observed_from,
                observed_until: snapshot.observed_until,
                records: records.len() as u32,
                sources: vec![coverage],
            }),
        };
        self.context = Some(captured.context);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        self.reader.verify_task_space_context(self.context.as_ref().ok_or_else(invalid_data)?).await
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
impl<R: NativeRead> TelemetrySource for TaskSpaceSource<R> {
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
