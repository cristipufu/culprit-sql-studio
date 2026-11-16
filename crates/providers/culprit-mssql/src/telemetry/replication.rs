//! Database replication snapshots reuse the scan's native families and the common frozen-source lifecycle.
use crate::{
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        max_snapshot_rows: crate::replication::MAX_REPLICAS as u32,
        ..CollectionBudget::upper_limits()
    })
}

pub(super) struct ReplicationSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<Reader<R>>,
}

struct Reader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    owner: Option<Context>,
}

impl ReplicationSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        Self::with_io(ReadSession::new(pool, request.budget.clone()), &request, host, port)
    }
}

impl<R: NativeRead> ReplicationSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication }) {
            return Err(invalid_data());
        }
        let reader = Reader { io, host, port, owner: None };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, request)? })
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = Context::read(&mut self.io, &self.host, self.port).await?;
        // Azure keeps its native pressure fence. This source-specific route does not relax the generic state
        // adapter's platform/role policy, and boxed SQL/MI never parse an Azure-only pressure view here.
        if before.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            crate::resources::admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        }
        let observed = crate::replication::read_selected(
            &mut self.io,
            before.activity.edition,
            request.budget.max_snapshot_rows as usize,
        )
        .await?;
        let after = before.recheck(&mut self.io, &self.host, self.port).await?;
        let context = before.completed(&after)?;
        let (records, sources) = observed.into_records(
            before.is_qualified() && after.is_qualified(),
            before.qualification_error.or(after.qualification_error),
        );
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: "replication-state".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Replication,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: records.len() as u32,
                sources,
            }),
        };
        self.owner = Some(after);
        Ok(culprit_read::FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let owner = self.owner.as_ref().ok_or_else(invalid_data)?;
        let after = owner.recheck(&mut self.io, &self.host, self.port).await?;
        match owner.completed(&after) {
            Ok(_) => Ok(true),
            Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for ReplicationSource<R> {
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
mod native_tests;
#[cfg(test)]
mod tests;
