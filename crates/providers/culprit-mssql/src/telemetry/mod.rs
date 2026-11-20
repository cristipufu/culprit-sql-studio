//! Native collection adapters over bounded statistics readers and stable parameterized queries.
mod backup_history;
mod cache_entry_memory;
mod cached_definitions;
mod cached_runtime;
mod cdc;
mod check_constraints;
mod cleanup;
mod columns;
mod columnstore_rowgroups;
mod compilation;
mod compiler_admission;
mod full_text_populations;
mod grants;
mod hyperscale_log_components;
mod index_operations;
mod indexes;
mod objects;
mod page_waits;
mod partitions;
mod recipes;
mod replication;
mod request_memory;
mod retained_definitions;
mod streamed;
mod table_memory;
mod task_space;
mod triggers;
use crate::query_store::{catalog_sql as sql, decode};
pub(crate) mod activity;
mod cursor;
mod definitions;
mod description;
mod history_acquisition;
pub(crate) use description::query_history_identity;
mod discovery;
pub(crate) use discovery::discover;
mod events;
mod foreign_keys;
mod point;
mod progress;
mod resource_rows;
mod resources;
pub(crate) use resources::discover_resources;
mod state;
mod statistics;
mod subject;
mod tables;

pub(crate) fn source(
    pool: Arc<Pool>,
    request: &CollectionRequest,
    host: String,
    port: u16,
) -> Result<Box<dyn TelemetrySource>> {
    recipes::open(pool, request, host, port)
}

#[cfg(test)]
pub(crate) fn page_wait_source_from_reader<R: crate::read::NativeRead + 'static>(
    reader: crate::activity::ActivityReader<R>,
    request: &CollectionRequest,
) -> Result<Box<dyn TelemetrySource>> {
    Ok(Box::new(page_waits::PageWaitSource::from_reader(reader, request)?))
}

pub(crate) fn qualification_source(
    pool: Arc<Pool>,
    request: &CollectionRequest,
    host: String,
    port: u16,
) -> Result<Box<dyn TelemetrySource>> {
    if request.collection()
        != culprit_core::telemetry::TelemetryCollection::Snapshot(
            culprit_core::telemetry::SnapshotDataset::QueryCompilation,
        )
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(Box::new(streamed::StreamedSnapshot::for_native_measurement(
        pool,
        request,
        host,
        port,
        compilation::QueryCompilation,
    )?))
}

pub(crate) fn recipes() -> Vec<culprit_core::telemetry::RecipeDescriptor> {
    recipes::declarations()
}

use crate::query_store::replicas::Population;
use crate::read::{Parameter, ReadError, ReadSession as Session};
use crate::{conn::Pool, value::Rec};
use async_trait::async_trait;
use culprit_core::{
    Result,
    telemetry::{
        CollectionDescription, CollectionRead, CollectionRequest, CollectionStop, QueryHistoryPopulationBinding,
        QueryHistoryPopulationBindingEntry, ReadWorkMetrics, SourceObservation, TelemetrySource, TelemetryStream,
    },
};
use cursor::{Cursor, PlanRange};
use std::collections::BTreeSet;
use std::sync::Arc;

pub(crate) struct Source<R = Session> {
    request: CollectionRequest,
    host: String,
    port: u16,
    session: R,
    description: Option<description::Description>,
    definitions: definitions::Definitions,
    progress: progress::PagedProgress,
    observed_runtime_groups: BTreeSet<i64>,
    observed_wait_groups: BTreeSet<i64>,
}

struct PageQuery {
    sql: String,
    parameters: Vec<Parameter>,
    range: Option<(Cursor, Population, PlanRange)>,
}

impl Source {
    pub(crate) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(Session::new(pool, request.budget.clone()), request, host, port)
    }
}
impl<R: crate::read::NativeRead> Source<R> {
    fn with_io(session: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        if request.collection() != culprit_core::telemetry::TelemetryCollection::QueryHistory {
            return Err(decode::malformed());
        }
        Ok(Self {
            request: request.clone(),
            host,
            port,
            session,
            description: None,
            definitions: definitions::Definitions::default(),
            progress: progress::PagedProgress::new(request.collection()),
            observed_runtime_groups: BTreeSet::new(),
            observed_wait_groups: BTreeSet::new(),
        })
    }
    async fn admitted(&mut self) -> std::result::Result<(), ReadError> {
        self.description
            .as_ref()
            .ok_or_else(decode::malformed)?
            .acquisition
            .admit(&mut self.session)
            .await
            .map_err(ReadError::Source)
    }
    async fn page(
        &mut self,
        stream: TelemetryStream,
        token: Option<&str>,
    ) -> std::result::Result<CollectionRead, ReadError> {
        if matches!(stream, TelemetryStream::QueryDefinitions | TelemetryStream::PlanDefinitions) {
            self.admitted().await?;
            let description = self.description.as_ref().ok_or_else(decode::malformed)?;
            return self
                .definitions
                .page(
                    &mut self.session,
                    &description.value,
                    &self.request.budget,
                    stream,
                    token,
                    description.layout.variants == crate::query_store::variants_sql::Availability::Readable,
                )
                .await;
        }
        if self.definitions.is_frozen() {
            return Err(decode::malformed().into());
        }
        let description = self.description.as_ref().ok_or_else(decode::malformed)?;
        if stream == TelemetryStream::QueryWaits && !description.waits_supported() {
            return Ok(CollectionRead::Stopped(CollectionStop::SourceUnavailable));
        }
        let Some(query) = self.prepare_page(stream, token)? else {
            return Ok(CollectionRead::Page { records: vec![], next_cursor: None });
        };
        self.admitted().await?;
        let rows = self.session.query(&query.sql, query.parameters, self.request.budget.page_rows as usize + 1).await?;
        self.finish_page(stream, rows, query.range).map_err(ReadError::Source)
    }

    fn prepare_page(&self, stream: TelemetryStream, token: Option<&str>) -> Result<Option<PageQuery>> {
        let description = self.description.as_ref().ok_or_else(decode::malformed)?;
        let interval = description
            .value
            .query_history()
            .ok_or_else(decode::malformed)?
            .interval
            .native_id
            .parse()
            .map_err(|_| decode::malformed())?;
        let mut parameters =
            vec![Parameter::Integer(i64::from(self.request.budget.page_rows) + 1), Parameter::Integer(interval)];
        let range = if let Some(token) = token {
            let mut cursor = Cursor::parse(
                token,
                stream,
                &description.value.query_history().ok_or_else(decode::malformed)?.interval.native_id,
            )?;
            match cursor.next_range(
                &description.replica_group_ids,
                description.plan_upper_bound,
                description.range_population(),
            )? {
                Some((population, range)) => {
                    if let Population::ReplicaGroup(group) = population {
                        parameters.push(Parameter::Integer(group))
                    }
                    parameters.extend([Parameter::Integer(range.lower), Parameter::Integer(range.upper)]);
                    Some((cursor, population, range))
                }
                None => return Ok(None),
            }
        } else {
            None
        };
        let selected = range.as_ref().map(|(_, population, _)| *population);
        let sql = match stream {
            TelemetryStream::QueryRuntime => sql::runtime(selected, &description.layout),
            TelemetryStream::QueryWaits => sql::waits(selected, &description.layout),
            _ => return Err(decode::malformed()),
        };
        Ok(Some(PageQuery { sql, parameters, range }))
    }
    fn finish_page(
        &mut self,
        stream: TelemetryStream,
        rows: Vec<Rec>,
        range: Option<(Cursor, Population, PlanRange)>,
    ) -> Result<CollectionRead> {
        let description = self.description.as_ref().ok_or_else(decode::malformed)?;
        let overflow = rows.len() > self.request.budget.page_rows as usize;
        let selected = range.as_ref().map(|(_, population, range)| (*population, range.clone()));
        let mut cursor = match range {
            Some((mut cursor, _population, range)) => {
                if overflow && let Err(reason) = cursor.split(range) {
                    return Ok(CollectionRead::Stopped(reason));
                }
                Some(cursor)
            }
            None if overflow => Some(Cursor::new(
                stream,
                description.value.query_history().ok_or_else(decode::malformed)?.interval.native_id.clone(),
                description.range_population(),
            )),
            None => None,
        };
        if overflow {
            return Ok(CollectionRead::Page {
                records: vec![],
                next_cursor: cursor.map(|cursor| cursor.encode()).transpose()?,
            });
        }
        let mut records = Vec::with_capacity(rows.len());
        for row in &rows {
            let population = Population::from_row(row)?;
            if description.layout.partitioned != matches!(population, Population::ReplicaGroup(_)) {
                return Ok(CollectionRead::Stopped(CollectionStop::SourceChanged));
            }
            if let Some((selected, range)) = &selected {
                let plan = decode::integer(row, "plan_id")?;
                if plan < range.lower
                    || plan >= range.upper
                    || matches!(selected, Population::ReplicaGroup(_)) && population != *selected
                {
                    return Ok(CollectionRead::Stopped(CollectionStop::SourceChanged));
                }
            }
            let definition = self.definitions.reference(row, &description.value)?;
            let record = decode::record(row, stream, definition)?;
            if matches!(&record, culprit_core::telemetry::TelemetryRecord::QueryRuntime(runtime)
                if runtime.page_server_reads.as_ref().is_none_or(|statistics| statistics.availability != description.layout.page_server_reads))
            {
                return Ok(CollectionRead::Stopped(CollectionStop::SourceChanged));
            }
            if !record.is_valid() {
                return Err(decode::malformed());
            }
            records.push(record);
        }
        if records.iter().any(|record| {
            record.measurement_key().is_none_or(|key| {
                !description.value.accepts_measurement(key)
                    || !key
                        .native
                        .qualifiers
                        .get("plan_id")
                        .is_some_and(|id| id.parse::<i64>().is_ok_and(|id| id < description.plan_upper_bound))
            })
        }) {
            return Ok(CollectionRead::Stopped(CollectionStop::SourceChanged));
        }
        if description.range_population() == cursor::PopulationRange::AllGroups {
            for row in &rows {
                let Population::ReplicaGroup(group) = Population::from_row(row)? else {
                    return Err(decode::malformed());
                };
                let groups = if stream == TelemetryStream::QueryRuntime {
                    &mut self.observed_runtime_groups
                } else {
                    &mut self.observed_wait_groups
                };
                groups.insert(group);
                if groups.len()
                    > culprit_core::MAX_RUNTIME_POPULATIONS
                        .min(culprit_core::telemetry::MAX_QUERY_HISTORY_DIMENSION_VALUES)
                {
                    return Ok(CollectionRead::Stopped(CollectionStop::RowLimit));
                }
            }
        }
        let next_cursor = if let Some(cursor) = cursor.take() {
            let mut remaining = cursor.clone();
            remaining
                .next_range(
                    &description.replica_group_ids,
                    description.plan_upper_bound,
                    description.range_population(),
                )?
                .map(|_| cursor.encode())
                .transpose()?
        } else {
            None
        };
        Ok(CollectionRead::Page { records, next_cursor })
    }
}

#[async_trait]
impl<R: crate::read::NativeRead> TelemetrySource for Source<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        let acquisition =
            history_acquisition::HistoryAcquisition::read(&mut self.session, &self.host, self.port).await?;
        acquisition.admit(&mut self.session).await?;
        let description = description::read(&mut self.session, &self.request, acquisition).await?;
        let value = description.value.clone();
        self.description = Some(description);
        self.progress.described();
        Ok(value)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.session.read_metrics()
    }
    fn query_count(&self) -> u32 {
        self.session.queries()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.progress.reading(stream, cursor)?;
        let result = match self.page(stream, cursor).await {
            Ok(page) => Ok(page),
            Err(ReadError::Stopped(reason)) => Ok(CollectionRead::Stopped(reason)),
            Err(ReadError::Source(error)) => {
                if let Some(culprit_core::ErrorDetail::CollectionStopped(reason)) = error.detail {
                    Ok(CollectionRead::Stopped(reason))
                } else {
                    Err(error)
                }
            }
        };
        self.progress.read(stream, result)
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        self.progress.verifying()?;
        self.admitted().await.map_err(ReadError::into_error)?;
        let before = self.description.as_ref().ok_or_else(decode::malformed)?;
        let mut current = description::read(&mut self.session, &self.request, before.acquisition.clone()).await?;
        let after = before.acquisition.recheck(&mut self.session, &self.host, self.port).await?;
        let owner = before.acquisition.completed(&after)?;
        let SourceObservation::QueryHistory(history) = &mut current.value.observation else {
            return Err(decode::malformed());
        };
        history.acquisition_owner = Some(Box::new(owner));
        // Revalidate every original native metadata fact before deriving membership from already drained rows.
        if !history.is_valid() {
            return Err(decode::malformed());
        }
        if before
            .value
            .completed(culprit_core::telemetry::SourceCompletion::QueryHistory(Box::new(history.clone())))
            .is_err()
        {
            return Ok(culprit_core::telemetry::SourceCompletion::Changed);
        }
        if before.range_population() == cursor::PopulationRange::AllGroups && !self.observed_runtime_groups.is_empty() {
            if !self.observed_wait_groups.is_subset(&self.observed_runtime_groups) {
                return Ok(culprit_core::telemetry::SourceCompletion::Changed);
            }
            let catalog = crate::query_store::replicas::observed_catalog(
                self.observed_runtime_groups.iter().copied().map(Population::ReplicaGroup),
                &history.interval.from,
                &history.interval.until,
            )?
            .native_facts();
            history.population_binding = QueryHistoryPopulationBinding::Dimensions {
                bindings: catalog
                    .populations
                    .iter()
                    .map(|population| QueryHistoryPopulationBindingEntry {
                        population: population.identity.clone(),
                        qualifiers: [("replica_group_id".into(), population.identity.id.clone())].into(),
                    })
                    .collect(),
            };
            history.dimensions.insert(
                "replica_group_id".into(),
                catalog.populations.iter().map(|population| population.identity.id.clone()).collect(),
            );
            history.population_catalog = Some(catalog);
        }
        self.progress.verified();
        Ok(culprit_core::telemetry::SourceCompletion::QueryHistory(Box::new(history.clone())))
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        self.progress.close();
        self.session.close().await
    }
}

#[cfg(test)]
mod history_tests;

#[cfg(test)]
mod native_history;
