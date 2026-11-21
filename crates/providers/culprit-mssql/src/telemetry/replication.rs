//! Database replication snapshots reuse the scan's native families and the common frozen-source lifecycle.
use crate::{
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

// Existing worst-case MI: 3 initial + 2 AG + 1 geo + 3 frame recheck + 3 final = 12 reader dispatches.
// Azure HS: 2 initial + 1 pressure + 1 geo + 1 layout + 1 topology + 1 frame + 1 final + 1 topology final = 9 (+1 local-state data query when that native layout is available).
const MAX_TOTAL_QUERIES: u32 = 13; // Reader envelope plus the existing one-session SET dispatch.

pub(super) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        max_snapshot_rows: crate::replication::MAX_REPLICAS as u32,
        max_queries: MAX_TOTAL_QUERIES,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
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
    replica_authority: Option<crate::replication::authority::Witness>,
}

impl ReplicationSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        Self::with_io(ReadSession::collection(pool, request.budget.clone())?, &request, host, port)
    }
}

impl<R: NativeRead> ReplicationSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.subject.require_database()?;
        if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication }) {
            return Err(invalid_data());
        }
        let reader = Reader { io, host, port, owner: None, replica_authority: None };
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
        let mut observed = crate::replication::read_selected(
            &mut self.io,
            before.activity.edition,
            request.budget.max_snapshot_rows as usize,
        )
        .await?;
        let scope = if before.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            let remaining = observed.remaining(request.budget.max_snapshot_rows as usize);
            let scope = crate::replication::authority::scope(&before).ok();
            let (parts, witness) =
                crate::replication::authority::selected(&mut self.io, &before, scope.as_ref(), remaining).await?;
            observed.parts.extend(parts);
            self.replica_authority = witness;
            scope
        } else {
            None
        };
        let after = before.recheck(&mut self.io, &self.host, self.port).await?;
        let context = before.completed(&after)?;
        let (records, sources) = observed.into_records(
            before.is_qualified() && after.is_qualified(),
            before.qualification_error.or(after.qualification_error),
        );
        if self
            .replica_authority
            .as_ref()
            .is_some_and(|witness| ObservationBounds::new(&witness.observed_at, &context.observed_until).is_none())
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::Replication,
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            records: records.len() as u32,
            sources,
        };
        let observation = match scope {
            Some(scope) => SourceObservation::MssqlReplication {
                snapshot,
                scope: Box::new(scope),
                database_writable: context.database_writable.ok_or_else(invalid_data)?,
            },
            None => SourceObservation::Snapshot(snapshot),
        };
        let description = CollectionDescription {
            target: context.target,
            partition: context.partition,
            source: "replication-state".into(),
            continuity: SourceContinuity::Unverified,
            observation,
        };
        self.owner = Some(after);
        Ok(culprit_read::FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let owner = self.owner.as_ref().ok_or_else(invalid_data)?;
        let after = owner.recheck(&mut self.io, &self.host, self.port).await?;
        match owner.completed(&after) {
            Ok(_) => match &self.replica_authority {
                Some(witness) => witness.verify(&mut self.io, &after).await,
                None => Ok(true),
            },
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
