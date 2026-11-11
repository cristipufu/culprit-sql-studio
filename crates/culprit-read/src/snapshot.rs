//! One paged snapshot lifecycle for every native provider. Drivers supply facts and completion evidence only.
mod buffer;
use crate::{ReadBatch, SourceProgress as Progress, invalid_source};
pub use buffer::SnapshotBuffer;
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct SnapshotRecord {
    pub source: String,
    pub record: TelemetryRecord,
    pub complete_scope: bool,
}
pub enum SnapshotScope {
    Available { source: String, complete: bool, error_code: Option<ErrorCode> },
    NotApplicable { source: String },
    Unavailable { source: String, error_code: ErrorCode },
}
impl SnapshotScope {
    pub fn complete(source: impl Into<String>) -> Self {
        Self::Available { source: source.into(), complete: true, error_code: None }
    }
    fn source(&self) -> &str {
        match self {
            Self::Available { source, .. } | Self::NotApplicable { source } | Self::Unavailable { source, .. } => {
                source
            }
        }
    }
}
pub struct SnapshotRecheck {
    pub unchanged: bool,
    pub observed_until: String,
    pub scopes: Vec<SnapshotScope>,
}
#[async_trait::async_trait]
pub trait SnapshotReader: Send {
    type Row: Send;
    async fn describe(&mut self, request: &CollectionRequest) -> Result<CollectionDescription>;
    async fn begin(&mut self, request: &CollectionRequest) -> Result<()>;
    async fn next_batch(&mut self) -> Result<ReadBatch<Self::Row>>;
    fn decode(&self, row: &Self::Row, description: &CollectionDescription) -> Result<SnapshotRecord>;
    async fn recheck(&mut self, description: &CollectionDescription) -> Result<SnapshotRecheck>;
    fn query_count(&self) -> u32;
    fn read_metrics(&self) -> Option<ReadWorkMetrics>;
    async fn close(&mut self) -> SourceShutdown;
}
#[derive(Default)]
struct Count {
    rows: u32,
    incomplete_scope: bool,
}
pub struct SnapshotSource<R: SnapshotReader> {
    reader: R,
    request: CollectionRequest,
    description: Option<CollectionDescription>,
    progress: Progress,
    rows: u32,
    counts: BTreeMap<String, Count>,
}
impl<R: SnapshotReader> SnapshotSource<R> {
    pub fn new(reader: R, request: &CollectionRequest) -> Result<Self> {
        request.validate()?;
        if !matches!(request.selection, CollectionSelection::Snapshot { .. }) {
            return Err(invalid_source());
        }
        Ok(Self {
            reader,
            request: request.clone(),
            description: None,
            progress: Progress::New,
            rows: 0,
            counts: BTreeMap::new(),
        })
    }
    /// Read-only access to provider diagnostics; lifecycle transitions stay owned here.
    pub fn reader(&self) -> &R {
        &self.reader
    }
    async fn page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        if !matches!(self.progress, Progress::Ready | Progress::Reading) {
            return Err(invalid_source());
        }
        let description = self.description.as_ref().ok_or_else(invalid_source)?;
        let SourceObservation::PagedSnapshot(declared) = &description.observation else {
            return Err(invalid_source());
        };
        if stream != declared.dataset.stream() {
            return Err(invalid_source());
        }
        let expected = (self.progress == Progress::Reading).then(|| self.rows.to_string());
        if cursor != expected.as_deref() {
            return Err(invalid_source());
        }
        let starting = self.progress == Progress::Ready;
        self.progress = Progress::Failed;
        if starting {
            self.reader.begin(&self.request).await?;
        }
        let batch = self.reader.next_batch().await?;
        if batch.rows.is_empty() && !batch.complete {
            return Err(invalid_source());
        }
        let decoded = batch.rows.iter().map(|row| self.reader.decode(row, description)).collect::<Result<Vec<_>>>()?;
        if decoded.iter().any(|row| {
            let source = match row.record.native_identity() {
                NativeRecordIdentity::Operational { source, .. }
                | NativeRecordIdentity::WaitCounter { source, .. }
                | NativeRecordIdentity::Replication { source, .. }
                | NativeRecordIdentity::Setting { source, .. }
                | NativeRecordIdentity::Tuning { source, .. }
                | NativeRecordIdentity::ObjectDefinition { source, .. } => Some(source),
                _ => None,
            };
            !declared.sources.contains(&row.source)
                || row.record.stream() != stream
                || source.as_deref() != Some(&row.source)
                || !row.record.is_valid()
        }) {
            return Err(invalid_source());
        }
        self.rows = self
            .rows
            .checked_add(decoded.len().try_into().map_err(|_| invalid_source())?)
            .ok_or_else(invalid_source)?;
        if self.rows > self.request.budget.max_snapshot_rows {
            return Err(CollectionStop::RowLimit.error());
        }
        let records = decoded
            .into_iter()
            .map(|row| {
                let count = self.counts.entry(row.source).or_default();
                count.rows += 1;
                count.incomplete_scope |= !row.complete_scope;
                row.record
            })
            .collect();
        self.progress = if batch.complete { Progress::Complete } else { Progress::Reading };
        Ok(CollectionRead::Page { records, next_cursor: (!batch.complete).then(|| self.rows.to_string()) })
    }
    fn coverage(
        &self,
        declared: &PagedSnapshotDescription,
        scopes: Vec<SnapshotScope>,
    ) -> Result<Vec<StateSourceCoverage>> {
        let names = scopes.iter().map(SnapshotScope::source).collect::<BTreeSet<_>>();
        if scopes.len() != names.len() || names != declared.sources.iter().map(String::as_str).collect() {
            return Err(invalid_source());
        }
        scopes
            .into_iter()
            .map(|scope| {
                let count = self.counts.get(scope.source());
                let rows = count.map_or(0, |count| count.rows);
                match scope {
                    SnapshotScope::NotApplicable { source } if rows == 0 => {
                        Ok(StateSourceCoverage::not_applicable(source))
                    }
                    SnapshotScope::NotApplicable { .. } => Err(invalid_source()),
                    SnapshotScope::Unavailable { source, error_code } if rows == 0 => {
                        Ok(StateSourceCoverage::unavailable(source, error_code, None))
                    }
                    SnapshotScope::Unavailable { .. } => Err(invalid_source()),
                    SnapshotScope::Available { source, complete, error_code } => {
                        let mut coverage = StateSourceCoverage::observed(
                            source,
                            rows,
                            complete && count.is_none_or(|count| !count.incomplete_scope),
                        );
                        coverage.error_code = error_code;
                        Ok(coverage)
                    }
                }
            })
            .collect()
    }
}
#[async_trait::async_trait]
impl<R: SnapshotReader> TelemetrySource for SnapshotSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        if self.progress != Progress::New {
            return Err(invalid_source());
        }
        self.progress = Progress::Failed;
        let description = self.reader.describe(&self.request).await?;
        if !matches!(description.observation, SourceObservation::PagedSnapshot(_))
            || !description.matches_request(&self.request)
        {
            return Err(invalid_source());
        }
        self.description = Some(description.clone());
        self.progress = Progress::Ready;
        Ok(description)
    }
    fn query_count(&self) -> u32 {
        self.reader.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        let result = self.page(stream, cursor).await;
        if result.is_err() {
            self.progress = Progress::Failed;
        }
        result
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        if self.progress != Progress::Complete {
            return Err(invalid_source());
        }
        self.progress = Progress::Failed;
        let description = self.description.as_ref().ok_or_else(invalid_source)?;
        let recheck = self.reader.recheck(description).await?;
        self.progress = Progress::Finished;
        if !recheck.unchanged {
            return Ok(SourceCompletion::Changed);
        }
        let SourceObservation::PagedSnapshot(declared) = &description.observation else {
            return Err(invalid_source());
        };
        Ok(SourceCompletion::Snapshot(StateSnapshotDescription {
            dataset: declared.dataset,
            observed_from: declared.observed_from.clone(),
            observed_until: recheck.observed_until,
            records: self.rows,
            sources: self.coverage(declared, recheck.scopes)?,
        }))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress = Progress::Closed;
        self.reader.close().await
    }
}
