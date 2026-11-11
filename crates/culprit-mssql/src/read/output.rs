//! SQL Server row accounting over the shared bounded delivery channel.
use super::{ReadError, ReadResult};
use crate::value::{Rec, Val};
use culprit_core::telemetry::CollectionStop;
use tokio::sync::mpsc;
pub(crate) type ReadBatch = culprit_read::ReadBatch<Rec>;
pub(super) struct Output(culprit_read::RowOutput<Rec>);
impl Output {
    pub fn buffered() -> Self {
        Self(culprit_read::RowOutput::buffered())
    }
    pub fn streaming(max_rows: usize, max_bytes: usize) -> (Self, mpsc::Receiver<ReadBatch>) {
        let (output, receiver) = culprit_read::RowOutput::streaming(max_rows, max_bytes);
        (Self(output), receiver)
    }
    pub async fn push(&mut self, row: Rec, bytes: usize) -> ReadResult<()> {
        self.0.push(row, bytes).await.map_err(|error| ReadError::Stopped(CollectionStop::from_error(&error)))
    }
    pub fn discarding(&self) -> bool {
        self.0.discarding()
    }
    pub fn retention_control(&self) -> culprit_read::RetentionControl {
        self.0.retention_control()
    }
    pub async fn finish(self) -> Vec<Rec> {
        self.0.finish().await
    }
}
pub(super) fn value_bytes(row: &Rec) -> usize {
    row.columns().fold(0usize, |total, (_, value)| {
        total.saturating_add(match value {
            Val::Text(value) => value.len(),
            Val::Bytes(value) => value.len(),
            _ => 16,
        })
    })
}
