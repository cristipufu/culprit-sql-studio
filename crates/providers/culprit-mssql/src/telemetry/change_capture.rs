//! One closed native log-capture owner. CDC and change feed retain distinct data and coverage.
use super::state;
use crate::{
    cdc, change_feed,
    conn::Pool,
    local_source::Context,
    read::{NativeRead, ReadError, ReadSession, invalid_data},
};
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Population {
    Scans,
    Errors,
}
impl Population {
    fn index(self) -> usize {
        match self {
            Self::Scans => 1,
            Self::Errors => 2,
        }
    }
    fn source(self, family: MssqlChangeCaptureFamily) -> &'static str {
        family.sources()[self.index()]
    }
    fn maximum(self, family: MssqlChangeCaptureFamily) -> usize {
        family.maximum(self.index())
    }
    async fn read(
        self,
        family: MssqlChangeCaptureFamily,
        reader: &mut impl NativeRead,
        database_id: u32,
        limit: usize,
    ) -> Result<Vec<Point>> {
        match (family, self) {
            (MssqlChangeCaptureFamily::Cdc, Self::Scans) => {
                Ok(cdc::scans(reader, database_id, limit).await?.into_iter().map(Point::Cdc).collect())
            }
            (MssqlChangeCaptureFamily::Cdc, Self::Errors) => {
                Ok(cdc::errors(reader, database_id, limit).await?.into_iter().map(Point::Cdc).collect())
            }
            (MssqlChangeCaptureFamily::ChangeFeed, Self::Scans) => {
                Ok(change_feed::scans(reader, database_id, limit).await?.into_iter().map(Point::ChangeFeed).collect())
            }
            (MssqlChangeCaptureFamily::ChangeFeed, Self::Errors) => {
                Ok(change_feed::errors(reader, database_id, limit).await?.into_iter().map(Point::ChangeFeed).collect())
            }
        }
    }
}
enum Point {
    Cdc(cdc::Point),
    ChangeFeed(change_feed::Point),
}
impl Point {
    fn bind(self, context: &NativeReadContext, position: u32) -> Result<OperationalObservation> {
        match self {
            Self::Cdc(point) => point.bind(context, position),
            Self::ChangeFeed(point) => point.bind(context, position),
        }
    }
}
enum Availability {
    Cdc(MssqlCdcStatus),
    ChangeFeed(MssqlChangeFeedAvailability),
}
impl Availability {
    fn from_point(point: &Point) -> Result<Self> {
        match point {
            Point::Cdc(cdc::Point { data: MssqlCdcData::Status(value), .. }) => Ok(Self::Cdc(value.clone())),
            Point::ChangeFeed(change_feed::Point { data: MssqlChangeFeedData::Availability(value), .. }) => {
                Ok(Self::ChangeFeed(value.clone()))
            }
            _ => Err(invalid_data()),
        }
    }
    fn unavailable(&self, population: Population, writable: Option<bool>) -> Option<StateSourceCoverage> {
        match self {
            Self::Cdc(status) => cdc_unavailable(population, status, writable),
            Self::ChangeFeed(status) => change_feed_unavailable(population, status, writable),
        }
    }
}
async fn availability(
    family: MssqlChangeCaptureFamily,
    reader: &mut impl NativeRead,
    context: &Context,
) -> Result<Point> {
    match family {
        MssqlChangeCaptureFamily::Cdc => cdc::status(reader, context).await.map(Point::Cdc),
        MssqlChangeCaptureFamily::ChangeFeed => change_feed::availability(reader, context).await.map(Point::ChangeFeed),
    }
}
fn family(selection: &CollectionSelection) -> Result<MssqlChangeCaptureFamily> {
    match selection {
        CollectionSelection::Snapshot { dataset: SnapshotDataset::Cdc } => Ok(MssqlChangeCaptureFamily::Cdc),
        CollectionSelection::Snapshot { dataset: SnapshotDataset::ChangeFeed } => {
            Ok(MssqlChangeCaptureFamily::ChangeFeed)
        }
        _ => Err(invalid_data()),
    }
}
const MAX_ROWS: u32 = 1 + MAX_MSSQL_CDC_SCAN_ROWS as u32 + MAX_MSSQL_CDC_ERROR_ROWS as u32;
// Original/final boxed authority 3+3, status 1, two pressure admissions, two DMV reads and end clock 1.
const MAX_READER_QUERIES: u32 = 12;
const MAX_QUERIES: u32 = MAX_READER_QUERIES + crate::read::ReadSession::SESSION_SETUP_QUERIES;
pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_ROWS),
        page_rows: requested.page_rows.min(32),
        page_bytes: requested.page_bytes.min(64 * 1024),
        total_bytes: requested.total_bytes.min(256 * 1024),
        max_pages: requested.max_pages.min(MAX_ROWS),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(super) struct ChangeCaptureSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<ChangeCaptureReader<R>>,
}
struct ChangeCaptureReader<R: NativeRead> {
    io: R,
    host: String,
    port: u16,
    context: Option<Context>,
    family: MssqlChangeCaptureFamily,
}
impl ChangeCaptureSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::collection(pool, bounded_budget(&request.budget))?, request, host, port)
    }
}
impl<R: NativeRead> ChangeCaptureSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        request.subject.require_database()?;
        let family = family(&request.selection)?;
        let mut request = request.clone();
        request.budget = bounded_budget(&request.budget);
        request.budget.validate()?;
        Ok(Self {
            inner: culprit_read::FrozenSource::new(
                ChangeCaptureReader { io, host, port, context: None, family },
                &request,
            )?,
        })
    }
}
impl<R: NativeRead> ChangeCaptureReader<R> {
    async fn admit(&mut self, context: &Context) -> Result<()> {
        if context.activity.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            crate::resources::admit_resources(&mut self.io).await
        } else {
            crate::resources::admit_instance_resources(&mut self.io).await
        }
        .map_err(ReadError::into_error)
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for ChangeCaptureReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let context = Context::read(&mut self.io, &self.host, self.port).await?;
        require_owner(&context)?;
        let mut points = Vec::new();
        let mut sources = Vec::new();
        let status = match availability(self.family, &mut self.io, &context).await {
            Ok(point) => {
                let status = Availability::from_point(&point)?;
                points.push((point, 1));
                sources.push(StateSourceCoverage::collected(self.family.sources()[0].into(), 1));
                Some(status)
            }
            Err(error) => {
                sources.push(state::source_failure(self.family.sources()[0].into(), error)?);
                None
            }
        };
        for population in [Population::Scans, Population::Errors] {
            let name = population.source(self.family);
            let maximum = population.maximum(self.family);
            if let Some(unavailable) =
                unavailable(self.family, population, status.as_ref(), sources.first(), context.activity.writable)
            {
                sources.push(unavailable);
                continue;
            }
            let remaining = (request.budget.max_snapshot_rows as usize).saturating_sub(points.len());
            if remaining == 0 {
                sources.push(StateSourceCoverage::unavailable(
                    name.into(),
                    CollectionStop::RowLimit.error().code,
                    Some(CollectionStop::RowLimit),
                ));
                continue;
            }
            self.admit(&context).await?;
            let limit = remaining.min(maximum);
            let result = population.read(self.family, &mut self.io, context.database_id, limit).await;
            match result {
                Ok(mut rows) => {
                    let complete = rows.len() <= limit;
                    rows.truncate(limit);
                    let mut coverage = StateSourceCoverage::observed(name.into(), rows.len() as u32, complete);
                    if !complete {
                        coverage.stopped = Some(CollectionStop::RowLimit);
                    }
                    sources.push(coverage);
                    points.extend(rows.into_iter().enumerate().map(|(index, row)| (row, index as u32 + 1)));
                }
                Err(error) => sources.push(state::source_failure(name.into(), error)?),
            }
        }
        let (until, _) = state::clock(&mut self.io).await?;
        if ObservationBounds::new(&context.activity.at, &until).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let read = NativeReadContext {
            target: context.activity.database.clone().into(),
            partition: context.source_partition()?,
            observed_from: context.activity.at.clone(),
            observed_until: until.clone(),
            database_writable: context.activity.writable,
        };
        let records = points
            .into_iter()
            .map(|(point, position)| point.bind(&read, position).map(TelemetryRecord::Operational))
            .collect::<Result<Vec<_>>>()?;
        let description = CollectionDescription {
            target: read.target,
            partition: read.partition,
            source: self.family.source().into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: self.family.dataset(),
                observed_from: read.observed_from,
                observed_until: until,
                records: records.len().try_into().map_err(|_| invalid_data())?,
                sources,
            }),
        };
        self.context = Some(context);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        let original = self.context.as_ref().ok_or_else(invalid_data)?;
        let after = original.recheck(&mut self.io, &self.host, self.port).await?;
        require_owner(&after)?;
        let until = &description.observation.snapshot().ok_or_else(invalid_data)?.observed_until;
        Ok(original.same_source(&after)
            && original.database_wide == after.database_wide
            && original.completed(&after).is_ok()
            && ObservationBounds::new(until, &after.activity.at).is_some())
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
fn require_owner(context: &Context) -> Result<()> {
    if !context.is_qualified()
        || !context.source_partition()?.is_qualified()
        || context.connection_generation().is_none_or(|generation| generation == 0)
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(())
}
fn unavailable(
    family: MssqlChangeCaptureFamily,
    population: Population,
    status: Option<&Availability>,
    status_source: Option<&StateSourceCoverage>,
    writable: Option<bool>,
) -> Option<StateSourceCoverage> {
    let Some(status) = status else {
        return Some(StateSourceCoverage::unavailable(
            population.source(family).into(),
            status_source.and_then(|source| source.error_code).unwrap_or(ErrorCode::QueryFailed),
            status_source.and_then(|source| source.stopped),
        ));
    };
    status.unavailable(population, writable)
}
fn cdc_unavailable(
    population: Population,
    status: &MssqlCdcStatus,
    writable: Option<bool>,
) -> Option<StateSourceCoverage> {
    let name = population.source(MssqlChangeCaptureFamily::Cdc);
    if status.enabled == Some(false) {
        return Some(StateSourceCoverage::not_applicable(name.into()));
    }
    if status.enabled.is_none() || writable.is_none() || status.has_permission.is_none() {
        let stop = CollectionStop::SourceUnavailable;
        return Some(StateSourceCoverage::unavailable(name.into(), stop.error().code, Some(stop)));
    }
    let layout = match population {
        Population::Scans => status.scan_layout_available,
        Population::Errors => status.error_layout_available,
    };
    let code = if writable != Some(true) {
        Some(ErrorCode::Unsupported)
    } else if status.has_permission != Some(true) {
        Some(ErrorCode::PermissionDenied)
    } else if !layout {
        Some(ErrorCode::Unsupported)
    } else {
        None
    };
    code.map(|code| StateSourceCoverage::unavailable(name.into(), code, None))
}
fn change_feed_unavailable(
    population: Population,
    status: &MssqlChangeFeedAvailability,
    writable: Option<bool>,
) -> Option<StateSourceCoverage> {
    let name = population.source(MssqlChangeCaptureFamily::ChangeFeed);
    if writable.is_none() || status.has_permission().is_none() {
        let stop = CollectionStop::SourceUnavailable;
        return Some(StateSourceCoverage::unavailable(name.into(), stop.error().code, Some(stop)));
    }
    let layout = match population {
        Population::Scans => status.scan_layout_available,
        Population::Errors => status.error_layout_available,
    };
    let supported = status.native_major >= 16 || matches!(status.native_edition, 5 | 8);
    let code = if !supported || writable != Some(true) {
        Some(ErrorCode::Unsupported)
    } else if status.has_permission() != Some(true) {
        Some(ErrorCode::PermissionDenied)
    } else if !layout {
        Some(ErrorCode::Unsupported)
    } else {
        None
    };
    code.map(|code| StateSourceCoverage::unavailable(name.into(), code, None))
}
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for ChangeCaptureSource<R> {
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
mod native_tests;
