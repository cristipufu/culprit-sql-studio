//! Explicit MI history is one frozen native frame. No ordinary scan or resource collection is broadened.
use super::*;
use crate::{
    instance,
    read::{NativeStreamRead, invalid_data},
    resources::managed_instance as native,
};
use culprit_core::{
    Error, ErrorCode,
    serialization::{JsonSizeError, json_size},
};

const MAX_QUERIES: u32 = 12;
fn native_limit(budget: &CollectionBudget) -> u32 {
    budget
        .max_snapshot_rows
        .min(MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS)
        .min(budget.page_rows.saturating_mul(budget.max_pages))
}
pub(in crate::telemetry) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS),
        page_rows: requested.page_rows.min(64),
        page_bytes: requested.page_bytes.min(128 * 1024),
        total_bytes: requested.total_bytes.min(2 * 1024 * 1024),
        max_pages: requested.max_pages.min(64),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(in crate::telemetry) struct ManagedInstanceResourceSource<R: NativeStreamRead = Session> {
    inner: culprit_read::FrozenSource<Reader<R>>,
}
struct Reader<R: NativeStreamRead> {
    io: R,
    original: Option<Original>,
}
struct Original {
    observation: instance::Observation,
    context: Option<instance::Context>,
    connection: u64,
}
impl ManagedInstanceResourceSource {
    pub(in crate::telemetry) fn new(pool: Arc<Pool>, request: &CollectionRequest) -> Result<Self> {
        Self::with_io(Session::collection(pool, bounded_budget(&request.budget))?, request)
    }
}
impl<R: NativeStreamRead> ManagedInstanceResourceSource<R> {
    fn with_io(io: R, request: &CollectionRequest) -> Result<Self> {
        request.validate()?;
        if !matches!(&request.subject,CollectionSubject::Shared {resource} if resource.engine==culprit_core::Engine::Mssql && resource.kind==SharedResourceKind::Instance)
            || request.collection() != TelemetryCollection::ResourceHistory
        {
            return Err(invalid_data());
        }
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        window(&request)?;
        Ok(Self { inner: culprit_read::FrozenSource::new(Reader { io, original: None }, &request)? })
    }
}
fn window(request: &CollectionRequest) -> Result<Window> {
    let (from, until) = request.bounds()?;
    if until - from > chrono::Duration::seconds(MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_WINDOW_SECONDS) {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.mssql_managed_instance.window")));
    }
    Window::new(from, until)
}
fn connection<R: NativeStreamRead>(reader: &R) -> Result<u64> {
    reader.connection_generation().filter(|value| *value > 0).ok_or_else(|| CollectionStop::SourceUnavailable.error())
}
fn require_connection<R: NativeStreamRead>(reader: &R, expected: u64) -> Result<()> {
    if connection(reader)? == expected { Ok(()) } else { Err(CollectionStop::SourceChanged.error()) }
}
fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}
#[async_trait]
impl<R: NativeStreamRead> culprit_read::ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = instance::observe(&mut self.io).await?;
        let owner = before.identity().ok_or_else(unsupported)?.clone();
        let target = CollectionTarget::Shared(owner.clone());
        if !request.subject.accepts(&target) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let generation = connection(&self.io)?;
        let layout = if before.edition == crate::probe::ENGINE_EDITION_MANAGED_INSTANCE {
            native::layout(&mut self.io, before.major).await?
        } else {
            native::Layout::unsupported()
        };
        require_connection(&self.io, generation)?;
        let selected = window(request)?;
        let (context, observed_at, coverage, native_rows, records) = if layout.available()
            && before.edition == crate::probe::ENGINE_EDITION_MANAGED_INSTANCE
        {
            let context = before.context(&mut self.io).await?.ok_or_else(unsupported)?;
            require_connection(&self.io, generation)?;
            let read = native::read(&mut self.io, &selected, &layout, native_limit(&request.budget), &context).await?;
            if ObservationBounds::new(&before.observed_at, &read.observed_at).is_none() {
                return Err(CollectionStop::SourceChanged.error());
            }
            let mut coverage = StateSourceCoverage::observed(
                MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(),
                read.records.len() as u32,
                !read.records.is_empty() && !read.limited,
            );
            if read.limited {
                coverage.stopped = Some(CollectionStop::RowLimit);
            }
            let records = read.records.into_iter().map(TelemetryRecord::ResourceSample).collect::<Vec<_>>();
            (Some(context), read.observed_at, coverage, read.native_rows, records)
        } else {
            let coverage = StateSourceCoverage::unavailable(
                MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(),
                layout.unavailable_code(),
                None,
            );
            (None, before.observed_at.clone(), coverage, 0, Vec::new())
        };
        require_connection(&self.io, generation)?;
        let current_reader = if let Some(context) = &context {
            Some(Box::new(NativeReadContext {
                target: target.clone(),
                partition: SourcePartition::InstanceLocal {
                    member: Some(owner),
                    epoch: Some(context.capture_epoch()?),
                },
                observed_from: observed_at.clone(),
                observed_until: observed_at.clone(),
                database_writable: None,
            }))
        } else {
            None
        };
        let description = CollectionDescription {
            target,
            source: MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(),
            partition: SourcePartition::Target,
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::MssqlManagedInstanceResources(Box::new(
                MssqlManagedInstanceResourceHistory {
                    from: selected.from.to_rfc3339(),
                    until: selected.until.to_rfc3339(),
                    native_engine_edition: before.edition,
                    observed_at,
                    current_reader,
                    coverage,
                    native_rows,
                    layout: layout.native,
                },
            )),
        };
        json_size(&(&description, &records), request.budget.total_bytes as usize).map_err(|error| match error {
            JsonSizeError::LimitExceeded => CollectionStop::ByteLimit.error(),
            JsonSizeError::Serialization => invalid_data(),
        })?;
        self.original = Some(Original { observation: before, context, connection: generation });
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let before = self.original.as_ref().ok_or_else(invalid_data)?;
        if connection(&self.io)? != before.connection {
            return Ok(false);
        }
        let after = instance::observe(&mut self.io).await?;
        let SourceObservation::MssqlManagedInstanceResources(history) = &description.observation else {
            return Err(invalid_data());
        };
        if connection(&self.io)? != before.connection
            || !before.observation.same_process(&after)
            || ObservationBounds::new(&history.observed_at, &after.observed_at).is_none()
        {
            return Ok(false);
        }
        if let Some(context) = &before.context {
            if after.context(&mut self.io).await?.as_ref() != Some(context) {
                return Ok(false);
            }
        }
        Ok(connection(&self.io)? == before.connection)
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
#[async_trait]
impl<R: NativeStreamRead> TelemetrySource for ManagedInstanceResourceSource<R> {
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
