//! Azure SQL collection adapters over bounded native statistics readers and stable parameterized queries.
mod cached_definitions;
mod cached_runtime;
mod check_constraints;
mod columns;
mod compilation;
mod grants;
mod index_operations;
mod indexes;
mod objects;
mod partitions;
mod retained_definitions;
mod streamed;
mod triggers;
use crate::query_store::{catalog_sql as sql, decode};
pub(crate) mod activity;
mod cursor;
mod definitions;
mod description;
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
mod tables;

pub(crate) fn source(
    pool: Arc<Pool>,
    request: &CollectionRequest,
    host: String,
    port: u16,
) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if let culprit_core::telemetry::CollectionSubject::Shared { resource } = &request.subject
        && resource.engine == culprit_core::Engine::Mssql
        && resource.kind == culprit_core::telemetry::SharedResourceKind::Pool
        && request.collection() == culprit_core::telemetry::TelemetryCollection::ResourceHistory
    {
        return resources::pool_source(pool, request);
    }
    request.subject.require_database()?;
    match request.collection() {
        culprit_core::telemetry::TelemetryCollection::Snapshot(
            SnapshotDataset::StatementCounters
            | SnapshotDataset::StatementDefinitions
            | SnapshotDataset::Sequences
            | SnapshotDataset::Cleanup,
        ) => Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        )),
        culprit_core::telemetry::TelemetryCollection::DeadlockHistory => {
            Ok(Box::new(events::EventSource::new(pool, request, host, port)?))
        }
        culprit_core::telemetry::TelemetryCollection::Activity => {
            Ok(Box::new(activity::ActivitySource::new(pool, request, host, port)?))
        }
        culprit_core::telemetry::TelemetryCollection::QueryHistory => {
            Ok(Box::new(Source::new(pool, request, host, port)?))
        }
        culprit_core::telemetry::TelemetryCollection::ResourceHistory => {
            Ok(Box::new(resources::ResourceSource::new(pool, request, host, port)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::StatementGrants) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, grants::Grants)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::IndexOperations) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, index_operations::IndexOperations)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Tables) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, tables::Tables)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::ForeignKeys) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, foreign_keys::ForeignKeys)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Columns) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, columns::Columns)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Statistics) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, statistics::Statistics)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Indexes) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, indexes::Indexes)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::CheckConstraints) => Ok(Box::new(
            streamed::StreamedSnapshot::new(pool, request, host, port, check_constraints::CheckConstraints)?,
        )),
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::QueryCompilation) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, compilation::QueryCompilation)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::CachedRuntime) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, cached_runtime::CachedRuntime)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Objects) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, objects::Objects)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Triggers) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, triggers::Triggers)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::Partitions) => {
            Ok(Box::new(streamed::StreamedSnapshot::new(pool, request, host, port, partitions::Partitions)?))
        }
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::CachedDefinitions) => Ok(Box::new(
            streamed::StreamedSnapshot::new(pool, request, host, port, cached_definitions::CachedDefinitions)?,
        )),
        culprit_core::telemetry::TelemetryCollection::Snapshot(SnapshotDataset::RetainedPlans) => Ok(Box::new(
            streamed::StreamedSnapshot::new(pool, request, host, port, retained_definitions::RetainedPlans)?,
        )),
        culprit_core::telemetry::TelemetryCollection::Snapshot(_) => {
            Ok(Box::new(state::StateSource::new(pool, request, host, port)?))
        }
    }
}

use crate::read::{Parameter, ReadError, ReadSession as Session};
use crate::{conn::Pool, value::Rec};
use async_trait::async_trait;
use culprit_core::{
    Result,
    telemetry::{
        CollectionDescription, CollectionRead, CollectionRequest, CollectionStop, ReadWorkMetrics, SnapshotDataset,
        TelemetrySource, TelemetryStream,
    },
};
use cursor::{Cursor, PlanRange};
use std::sync::Arc;

const MAX_SAMPLE_AGE_SECONDS: i64 = 60;
const PRESSURE_CPU_PERCENT: f64 = 70.0;
const PRESSURE_WORKER_PERCENT: f64 = 50.0;
const PRESSURE_IO_PERCENT: f64 = 80.0;

pub(crate) struct Source<R = Session> {
    request: CollectionRequest,
    host: String,
    port: u16,
    session: R,
    description: Option<description::Description>,
    definitions: definitions::Definitions,
    progress: progress::PagedProgress,
}

struct PageQuery {
    sql: String,
    parameters: Vec<Parameter>,
    range: Option<(Cursor, PlanRange)>,
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
        })
    }
    async fn admitted(&mut self) -> std::result::Result<(), ReadError> {
        admit_resources(&mut self.session).await
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
                .page(&mut self.session, &description.value, &self.request.budget, stream, token)
                .await;
        }
        if self.definitions.is_frozen() {
            return Err(decode::malformed().into());
        }
        let description = self.description.as_ref().ok_or_else(decode::malformed)?;
        if stream == TelemetryStream::QueryWaits && !description.waits_capture_enabled {
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
            match cursor.next_range(&description.replica_group_ids, description.plan_upper_bound)? {
                Some((replica, range)) => {
                    parameters.extend([
                        Parameter::Integer(replica),
                        Parameter::Integer(range.lower),
                        Parameter::Integer(range.upper),
                    ]);
                    Some((cursor, range))
                }
                None => return Ok(None),
            }
        } else {
            None
        };
        let sql = match (stream, range.is_some()) {
            (TelemetryStream::QueryRuntime, false) => sql::runtime(false),
            (TelemetryStream::QueryRuntime, true) => sql::runtime(true),
            (TelemetryStream::QueryWaits, false) => sql::waits(false),
            (TelemetryStream::QueryWaits, true) => sql::waits(true),
            _ => return Err(decode::malformed()),
        };
        Ok(Some(PageQuery { sql, parameters, range }))
    }
    fn finish_page(
        &mut self,
        stream: TelemetryStream,
        rows: Vec<Rec>,
        range: Option<(Cursor, PlanRange)>,
    ) -> Result<CollectionRead> {
        let description = self.description.as_ref().ok_or_else(decode::malformed)?;
        let overflow = rows.len() > self.request.budget.page_rows as usize;
        let mut cursor = match range {
            Some((mut cursor, range)) => {
                if overflow && let Err(reason) = cursor.split(range) {
                    return Ok(CollectionRead::Stopped(reason));
                }
                Some(cursor)
            }
            None if overflow => Some(Cursor::new(
                stream,
                description.value.query_history().ok_or_else(decode::malformed)?.interval.native_id.clone(),
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
            let definition = self.definitions.reference(row, &description.value)?;
            records.push(decode::record(row, stream, definition)?);
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
        let next_cursor = if let Some(cursor) = cursor.take() {
            let mut remaining = cursor.clone();
            remaining
                .next_range(&description.replica_group_ids, description.plan_upper_bound)?
                .map(|_| cursor.encode())
                .transpose()?
        } else {
            None
        };
        Ok(CollectionRead::Page { records, next_cursor })
    }
}

async fn admit_resources<R: crate::read::NativeRead>(reader: &mut R) -> std::result::Result<(), ReadError> {
    let latest = crate::resources::latest(reader).await.map_err(|error| match error {
        ReadError::Source(ref source)
            if matches!(
                source.detail,
                Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
            ) =>
        {
            ReadError::Stopped(CollectionStop::PressureUnavailable)
        }
        error => error,
    })?;
    pressure_admission(latest.as_ref()).map_err(ReadError::Stopped)
}
fn pressure_admission(latest: Option<&crate::resources::Latest>) -> std::result::Result<(), CollectionStop> {
    use crate::resources::Metric;
    let latest = latest.ok_or(CollectionStop::PressureUnavailable)?;
    let age = latest.age_seconds().ok_or(CollectionStop::PressureUnavailable)?;
    if age > MAX_SAMPLE_AGE_SECONDS {
        return Err(CollectionStop::PressureUnavailable);
    }
    for (metric, threshold) in [
        (Metric::Cpu, PRESSURE_CPU_PERCENT),
        (Metric::InstanceCpu, PRESSURE_CPU_PERCENT),
        (Metric::Workers, PRESSURE_WORKER_PERCENT),
        (Metric::DataIo, PRESSURE_IO_PERCENT),
        (Metric::LogWrite, PRESSURE_IO_PERCENT),
    ] {
        let value = latest.sample.value(metric).ok_or(CollectionStop::PressureUnavailable)?;
        if value >= threshold {
            return Err(CollectionStop::Pressure);
        }
    }
    Ok(())
}

#[async_trait]
impl<R: crate::read::NativeRead> TelemetrySource for Source<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        description::ensure_supported(&mut self.session).await?;
        self.admitted().await.map_err(ReadError::into_error)?;
        let description = description::read(&mut self.session, &self.request, &self.host, self.port).await?;
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
        let current = description::read(&mut self.session, &self.request, &self.host, self.port).await?;
        self.progress.verified();
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(self.description.as_ref().is_some_and(|before| {
            before.value == current.value && before.physical_incarnation == current.physical_incarnation
        })))
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        self.progress.close();
        self.session.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::*, value::Val};
    #[tokio::test]
    async fn database_cpu_saturation_refuses_new_source_work_even_when_host_cpu_is_low() {
        let row = replace(
            &latest_row("2020-01-01T06:01:59Z"),
            &[
                ("avg_cpu_percent", Val::Float(95.0)),
                ("avg_instance_cpu_percent", Val::Float(10.0)),
                ("max_worker_percent", Val::Float(0.0)),
                ("avg_data_io_percent", Val::Float(0.0)),
                ("avg_log_write_percent", Val::Float(0.0)),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![row])]);
        assert!(matches!(admit_resources(&mut io).await, Err(ReadError::Stopped(CollectionStop::Pressure))));
        assert_eq!(io.calls.len(), 1, "one pressure observation, with no retry");
    }

    #[tokio::test]
    async fn pressure_admission_uses_native_scope_and_rejects_missing_stale_invalid_or_future_samples() {
        let healthy = replace(
            &latest_row("2020-01-01T06:01:59Z"),
            &[
                ("avg_instance_cpu_percent", Val::Float(20.0)),
                ("max_worker_percent", Val::Float(0.0)),
                ("avg_data_io_percent", Val::Float(0.0)),
                ("avg_log_write_percent", Val::Float(0.0)),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![healthy.clone()])]);
        assert!(admit_resources(&mut io).await.is_ok());
        assert_eq!(io.calls.len(), 1);
        for rows in [
            vec![],
            vec![replace(&healthy, &[("sample_at", Val::Text("2020-01-01T06:00:59Z".into()))])],
            vec![replace(&healthy, &[("avg_instance_cpu_percent", Val::Null)])],
            vec![replace(&healthy, &[("avg_cpu_percent", Val::Null)])],
            vec![replace(&healthy, &[("avg_instance_cpu_percent", Val::Float(f64::NAN))])],
            vec![replace(&healthy, &[("sample_at", Val::Text("2020-01-01T06:02:00.0000001Z".into()))])],
        ] {
            let mut io = FakeRead::new(vec![Ok(rows)]);
            assert!(matches!(
                admit_resources(&mut io).await,
                Err(ReadError::Stopped(CollectionStop::PressureUnavailable))
            ));
        }
        let mut io = FakeRead::new(vec![Ok(vec![replace(
            &healthy,
            &[("avg_instance_cpu_percent", Val::Float(80.0)), ("avg_cpu_percent", Val::Float(0.0))],
        )])]);
        assert!(matches!(admit_resources(&mut io).await, Err(ReadError::Stopped(CollectionStop::Pressure))));
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::Cancelled))]);
        assert!(matches!(admit_resources(&mut io).await, Err(ReadError::Stopped(CollectionStop::Cancelled))));
    }
}

#[cfg(test)]
mod history_tests;
