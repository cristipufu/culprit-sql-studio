//! Capture a bounded file observation, close its read transaction, then use the common snapshot pager.
use crate::{
    file::FileIdentity,
    native::{
        self,
        state::{capture, source_name},
    },
    read::{FileAccounting, FileReader},
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use culprit_read::{
    ReadBatch, SnapshotBuffer, SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource,
};
use std::{path::PathBuf, time::Duration};

const METADATA_BUDGET: Duration = Duration::from_millis(250);

pub(crate) fn source(
    reader: FileReader,
    path: PathBuf,
    request: &CollectionRequest,
) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    let TelemetryCollection::Snapshot(dataset) = request.collection() else {
        return Err(unsupported());
    };
    if !matches!(
        dataset,
        SnapshotDataset::Settings
            | SnapshotDataset::Operational
            | SnapshotDataset::Tables
            | SnapshotDataset::Indexes
            | SnapshotDataset::Columns
            | SnapshotDataset::ForeignKeys
            | SnapshotDataset::Objects
            | SnapshotDataset::Triggers
            | SnapshotDataset::Statistics
            | SnapshotDataset::Sequences
    ) {
        return Err(unsupported());
    }
    Ok(Box::new(SnapshotSource::new(
        Reader {
            reader,
            path,
            dataset,
            records: None,
            identity: None,
            accounting: FileAccounting::new(request.budget.max_queries),
            observed_until: String::new(),
            scope: None,
        },
        request,
    )?))
}
fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}
struct Reader {
    reader: FileReader,
    path: PathBuf,
    dataset: SnapshotDataset,
    records: Option<SnapshotBuffer>,
    identity: Option<FileIdentity>,
    accounting: FileAccounting,
    observed_until: String,
    scope: Option<SnapshotScope>,
}
#[async_trait::async_trait]
impl SnapshotReader for Reader {
    type Row = SnapshotRecord;
    async fn describe(&mut self, request: &CollectionRequest) -> Result<CollectionDescription> {
        let dataset = self.dataset;
        let budget = request.budget.clone();
        // The pre-open handle is retained through SQLite close. Aliases bind one native file; replacement never
        // silently inherits an existing file's identity, including on platforms without Unix dev/inode metadata.
        let path = self.path.clone();
        let identity = tokio::task::spawn_blocking(move || FileIdentity::read(&path))
            .await
            .map_err(|error| crate::task_stopped(&error))??;
        let duration = METADATA_BUDGET
            .min(Duration::from_millis(budget.total_timeout_ms.into()))
            .min(Duration::from_millis(budget.statement_timeout_ms.into()));
        let captured = self
            .reader
            .with_accounting(duration, self.accounting.clone(), move |conn, path, accounting| {
                capture(conn, path, identity, dataset, &budget, accounting)
            })
            .await?;
        self.records = Some(SnapshotBuffer::new(
            captured
                .records
                .into_iter()
                .map(|record| SnapshotRecord { source: source_name(self.dataset).into(), record, complete_scope: true })
                .collect(),
            &request.budget,
        )?);
        self.identity = Some(captured.identity);
        self.observed_until = captured.until;
        self.scope = Some(captured.scope);
        Ok(captured.description)
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        Ok(())
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<SnapshotRecord>> {
        self.records.as_mut().ok_or_else(native::invalid)?.next_batch()
    }
    fn decode(&self, row: &SnapshotRecord, _: &CollectionDescription) -> Result<SnapshotRecord> {
        Ok(row.clone())
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        Ok(SnapshotRecheck {
            unchanged: self.identity.as_ref().is_some_and(|identity| identity.unchanged(&self.path)),
            observed_until: self.observed_until.clone(),
            scopes: vec![self.scope.take().ok_or_else(native::invalid)?],
        })
    }
    fn query_count(&self) -> u32 {
        self.accounting.snapshot().0
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.accounting.snapshot().1
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.shutdown()
    }
}
#[cfg(test)]
mod tests;
