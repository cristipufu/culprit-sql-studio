//! Bounded row delivery is independent of a native protocol. Every emitted page stays provisional until the driver
//! has consumed its completion response. This crate owns no connections, schedules, secrets or destination state.
use culprit_core::{Result, telemetry::CollectionStop};
use tokio::sync::mpsc;

mod fence;
mod frozen;
mod progress;
pub use frozen::{FrozenObservation, FrozenSource, ObservationReader};
pub use progress::{PagedProgress, SourceProgress};
mod snapshot;
pub use fence::ReadFence;
pub use snapshot::{SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource};

const BUFFERED_BATCHES: usize = 1;

pub struct ReadBatch<T> {
    pub rows: Vec<T>,
    pub complete: bool,
}
pub enum RowOutput<T> {
    Buffered(Vec<T>),
    Streaming(Batches<T>),
}
pub struct Batches<T> {
    sender: mpsc::Sender<ReadBatch<T>>,
    rows: Vec<T>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    discarding: bool,
}
impl<T> RowOutput<T> {
    pub fn buffered() -> Self {
        Self::Buffered(Vec::new())
    }
    pub fn streaming(max_rows: usize, max_bytes: usize) -> (Self, mpsc::Receiver<ReadBatch<T>>) {
        let (sender, receiver) = mpsc::channel(BUFFERED_BATCHES);
        (Self::Streaming(Batches { sender, rows: vec![], bytes: 0, max_rows, max_bytes, discarding: false }), receiver)
    }
    pub async fn push(&mut self, row: T, bytes: usize) -> Result<()> {
        match self {
            Self::Buffered(rows) => rows.push(row),
            Self::Streaming(batch) => {
                if batch.discarding || batch.sender.is_closed() {
                    batch.discarding = true;
                    batch.rows.clear();
                    batch.bytes = 0;
                    return Ok(());
                }
                if bytes > batch.max_bytes || batch.max_rows == 0 {
                    return Err(CollectionStop::ByteLimit.error());
                }
                if !batch.rows.is_empty()
                    && (batch.rows.len() >= batch.max_rows
                        || batch.bytes.checked_add(bytes).is_none_or(|sum| sum > batch.max_bytes))
                {
                    batch.flush(false).await;
                    if batch.discarding {
                        return Ok(());
                    }
                }
                batch.rows.push(row);
                batch.bytes += bytes;
            }
        }
        Ok(())
    }
    pub fn discard(&mut self) {
        match self {
            Self::Buffered(rows) => rows.clear(),
            Self::Streaming(batch) => {
                batch.discarding = true;
                batch.rows.clear();
                batch.bytes = 0;
            }
        }
    }
    pub fn discarding(&self) -> bool {
        matches!(self,Self::Streaming(batch) if batch.discarding || batch.sender.is_closed())
    }
    pub async fn finish(self) -> Vec<T> {
        match self {
            Self::Buffered(rows) => rows,
            Self::Streaming(mut batch) => {
                batch.flush(true).await;
                vec![]
            }
        }
    }
}
impl<T> Batches<T> {
    async fn flush(&mut self, complete: bool) {
        let rows = std::mem::take(&mut self.rows);
        self.bytes = 0;
        // The session pairs receiver release with native cancellation. Let the existing decoder drain within its
        // bounded grace period instead of losing normal completion that raced with cancellation.
        if !self.discarding && self.sender.send(ReadBatch { rows, complete }).await.is_err() {
            self.discarding = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(n: u64) -> u64 {
        n
    }
    #[tokio::test]
    async fn the_final_batch_is_held_until_completion_and_buffered_delivery_keeps_the_same_rows() {
        let (mut streaming, mut receiver) = RowOutput::streaming(2, 1024);
        let mut buffered = RowOutput::buffered();
        for n in [1, 2] {
            streaming.push(row(n), 16).await.unwrap();
            buffered.push(row(n), 16).await.unwrap();
        }
        assert!(matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        streaming.push(row(3), 16).await.unwrap();
        buffered.push(row(3), 16).await.unwrap();
        let first = receiver.recv().await.unwrap();
        assert_eq!(first.rows.len(), 2);
        assert!(!first.complete);
        assert!(streaming.finish().await.is_empty());
        let last = receiver.recv().await.unwrap();
        assert_eq!(last.rows.len(), 1);
        assert!(last.complete);
        let streamed: Vec<_> = first.rows.into_iter().chain(last.rows).collect();
        let buffered: Vec<_> = buffered.finish().await.into_iter().collect();
        assert_eq!(streamed, buffered);
    }
    #[tokio::test]
    async fn channel_backpressure_is_bounded_and_releasing_the_consumer_switches_to_discarded_delivery() {
        let (mut output, receiver) = RowOutput::streaming(1, 1024);
        output.push(row(1), 16).await.unwrap();
        output.push(row(2), 16).await.unwrap();
        let pending = output.push(row(3), 16);
        tokio::pin!(pending);
        assert!(futures_util::poll!(&mut pending).is_pending());
        drop(receiver);
        pending.await.unwrap();
    }
    #[tokio::test]
    async fn row_bytes_limit_chunks_and_an_oversized_row_cannot_enter_the_channel() {
        let (mut output, mut receiver) = RowOutput::streaming(10, 16);
        output.push(row(1), 16).await.unwrap();
        output.push(row(2), 16).await.unwrap();
        let first = receiver.recv().await.unwrap();
        assert_eq!(first.rows.len(), 1);
        assert!(!first.complete);
        assert_eq!(
            CollectionStop::from_error(&output.push(row(3), 17).await.err().unwrap()),
            CollectionStop::ByteLimit
        );
    }
}

/// Whole-response bounds are independent of page boundaries and native row representation.
pub struct ReadBudget {
    rows: usize,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
}
impl ReadBudget {
    pub fn new(max_rows: usize, max_bytes: usize) -> Self {
        Self { rows: 0, bytes: 0, max_rows, max_bytes }
    }
    pub fn row_started(&mut self) -> Result<()> {
        if self.rows >= self.max_rows {
            return Err(CollectionStop::RowLimit.error());
        }
        self.rows += 1;
        Ok(())
    }
    pub fn charge_values(&mut self, bytes: usize) -> Result<()> {
        let total = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| CollectionStop::ByteLimit.error())?;
        self.bytes = total;
        Ok(())
    }
}

fn invalid_source() -> culprit_core::Error {
    culprit_core::Error::new(culprit_core::ErrorCode::QueryFailed, culprit_core::t!("collection.invalid_source"))
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn response_limits_accumulate_across_pages_and_allow_the_exact_boundary() {
        let mut budget = ReadBudget::new(2, 10);
        budget.row_started().unwrap();
        budget.charge_values(4).unwrap();
        budget.row_started().unwrap();
        budget.charge_values(6).unwrap();
        assert_eq!(CollectionStop::from_error(&budget.row_started().unwrap_err()), CollectionStop::RowLimit);
        assert_eq!(CollectionStop::from_error(&budget.charge_values(1).unwrap_err()), CollectionStop::ByteLimit);
    }
    #[test]
    fn accounting_overflow_is_a_limit_failure_even_with_the_largest_budget() {
        let mut budget = ReadBudget::new(1, usize::MAX);
        budget.charge_values(usize::MAX).unwrap();
        assert_eq!(CollectionStop::from_error(&budget.charge_values(1).unwrap_err()), CollectionStop::ByteLimit);
        assert_eq!(
            CollectionStop::from_error(&ReadBudget::new(0, 0).row_started().unwrap_err()),
            CollectionStop::RowLimit
        );
    }
}
