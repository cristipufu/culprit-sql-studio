//! Native multi-source recipes use the same snapshot lifecycle and source-by-source coverage as every provider.
use super::*;
use crate::native::operational::Kind;
use culprit_read::SnapshotBuffer;
struct OperationalReader {
    io: ReadSession,
    host: String,
    port: u16,
    metadata: Option<Metadata>,
    budget: CollectionBudget,
    rows: Option<SnapshotBuffer>,
    scopes: Vec<SnapshotScope>,
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    Ok(Box::new(SnapshotSource::new(
        OperationalReader {
            host: connector.host.clone(),
            port: connector.port,
            io: ReadSession::for_engine(connector, request.budget.clone(), Engine::Yugabyte),
            metadata: None,
            budget: request.budget.clone(),
            rows: None,
            scopes: vec![],
        },
        request,
    )?))
}
#[async_trait::async_trait]
impl SnapshotReader for OperationalReader {
    type Row = SnapshotRecord;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        let meta = metadata::read(&mut self.io, &self.host, self.port).await?;
        let description = CollectionDescription {
            database: meta.database.clone(),
            source: format!("ysql-operational-{}", meta.node),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: meta.probe.now.clone(),
                sources: Kind::ALL.iter().map(|kind| kind.source().into()).collect(),
            }),
        };
        self.metadata = Some(meta);
        Ok(description)
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        let meta = self.metadata.as_ref().ok_or_else(native::invalid)?;
        let mut rows = vec![];
        for &kind in Kind::ALL {
            let source = kind.source();
            let notices = self.io.notices();
            match kind.read(&mut self.io, meta, &self.budget).await {
                Ok((records, complete)) => {
                    let complete = complete && self.io.notices() == notices;
                    rows.extend(records.into_iter().map(|record| SnapshotRecord {
                        source: source.into(),
                        record,
                        complete_scope: complete,
                    }));
                    if rows.len() > self.budget.max_snapshot_rows as usize {
                        return Err(CollectionStop::RowLimit.error());
                    }
                    self.scopes.push(SnapshotScope::Available { source: source.into(), complete, error_code: None });
                }
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::PermissionDenied | ErrorCode::Unsupported | ErrorCode::QueryFailed
                    ) && error.detail.is_none() =>
                {
                    self.scopes.push(SnapshotScope::Unavailable { source: source.into(), error_code: error.code });
                }
                Err(error) => return Err(error),
            }
        }
        self.rows = Some(SnapshotBuffer::new(rows, &self.budget)?);
        Ok(())
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<SnapshotRecord>> {
        self.rows.as_mut().ok_or_else(native::invalid)?.next_batch()
    }
    fn decode(&self, row: &SnapshotRecord, _: &CollectionDescription) -> Result<SnapshotRecord> {
        Ok(row.clone())
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        let after = metadata::read(&mut self.io, &self.host, self.port).await?;
        Ok(SnapshotRecheck {
            unchanged: self.metadata.as_ref().is_some_and(|before| before.same_source(&after)),
            observed_until: after.probe.now,
            scopes: std::mem::take(&mut self.scopes),
        })
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
