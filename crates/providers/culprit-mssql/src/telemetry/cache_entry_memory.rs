//! Selector-only cache observations use one source owner, frozen paging and the shared final binding proof.
use crate::{
    cache_entry_memory::{self, Point},
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadSession, invalid_data},
    relational_inputs::Bindings,
    sources,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use std::sync::Arc;

pub(super) struct CacheEntryMemorySource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<EntryReader<R>>,
}

struct EntryReader<R: NativeRead> {
    reader: R,
    host: String,
    port: u16,
    focus: CacheEntryMemoryFocus,
    budget: CollectionBudget,
    before: Option<Context>,
    point: Option<Point>,
    bindings: Bindings,
}

impl CacheEntryMemorySource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let request = bounded_request(request)?;
        Self::from_reader(ReadSession::new(pool, request.budget.clone()), &request, host, port)
    }
}

fn bounded_request(request: &CollectionRequest) -> Result<CollectionRequest> {
    request.validate()?;
    request.subject.require_database()?;
    let CollectionSelection::CacheEntryMemory { focus } = &request.selection else { return Err(invalid_data()) };
    if !focus.is_valid() {
        return Err(invalid_data());
    }
    let mut request = request.clone();
    request.budget = cache_entry_memory::bounded_budget(&request.budget);
    request.budget.validate()?;
    let CollectionSelection::CacheEntryMemory { focus } = &request.selection else { return Err(invalid_data()) };
    cache_entry_memory::validate(focus, &request.budget)?;
    Ok(request)
}

impl<R: NativeRead> CacheEntryMemorySource<R> {
    fn from_reader(reader: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let request = bounded_request(request)?;
        let CollectionSelection::CacheEntryMemory { focus } = &request.selection else { return Err(invalid_data()) };
        let reader = EntryReader {
            reader,
            host,
            port,
            focus: focus.as_ref().clone(),
            budget: request.budget.clone(),
            before: None,
            point: None,
            bindings: Bindings::default(),
        };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, &request)? })
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for EntryReader<R> {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let before = Context::read(&mut self.reader, &self.host, self.port).await?;
        if before.activity.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.azure_sql_only")));
        }
        crate::read::require_primary(before.activity.writable)?;
        if !before.is_qualified()
            || before.activity.database.telemetry_id()? != self.focus.context.target.telemetry_id()?
            || before.source_partition()? != self.focus.context.partition
            || self.focus.context.database_writable.is_some_and(|role| before.activity.writable != Some(role))
            || self.focus.statements.iter().any(|statement| statement.database_id != before.database_id)
            || ObservationBounds::new(&self.focus.context.observed_until, &before.activity.at).is_none()
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let point = cache_entry_memory::read(&mut self.reader, &self.focus, &self.budget).await?;
        if ObservationBounds::new(&before.activity.at, &point.at).is_none()
            || ObservationBounds::new(&self.focus.context.observed_until, &point.at).is_none()
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let mut records = Vec::new();
        for entry in &point.entries {
            for statement in &entry.statements {
                if !self.bindings.admit(&statement.context)? {
                    return Err(CollectionStop::RowLimit.error());
                }
            }
            records.push(TelemetryRecord::Operational(OperationalObservation {
                source: sources::CACHE_ENTRY_MEMORY.name.into(),
                key: entry.key()?,
                fact: OperationalFact::MssqlCacheEntryMemory(Box::new(entry.clone())),
            }));
        }
        let description = CollectionDescription {
            target: before.activity.database.clone().into(),
            partition: before.source_partition()?,
            source: sources::CACHE_ENTRY_MEMORY.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::CacheEntryMemory {
                focus: Box::new(self.focus.clone()),
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::CacheEntryMemory,
                    observed_from: point.at.clone(),
                    observed_until: point.at.clone(),
                    records: records.len() as u32,
                    sources: vec![StateSourceCoverage::observed(
                        sources::CACHE_ENTRY_MEMORY.name.into(),
                        records.len() as u32,
                        false,
                    )],
                },
            },
        };
        self.point = Some(point);
        self.before = Some(before);
        Ok(culprit_read::FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let original = self.point.as_ref().ok_or_else(invalid_data)?;
        let current = cache_entry_memory::read(&mut self.reader, &self.focus, &self.budget).await?;
        if !cache_entry_memory::same_bindings(original, &current)
            || ObservationBounds::new(&original.at, &current.at).is_none()
        {
            return Ok(false);
        }
        self.bindings.require_verified(&mut self.reader).await?;
        let before = self.before.as_ref().ok_or_else(invalid_data)?;
        let after = before.recheck(&mut self.reader, &self.host, self.port).await?;
        match before.completed(&after) {
            Ok(context) => Ok(context.has_qualified_local_owner()
                && ObservationBounds::new(&current.at, &context.observed_until).is_some()),
            Err(error) if CollectionStop::from_error(&error) == CollectionStop::SourceChanged => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn query_count(&self) -> u32 {
        self.reader.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}

#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for CacheEntryMemorySource<R> {
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
