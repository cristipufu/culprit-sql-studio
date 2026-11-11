//! Bounded row delivery shares one native decoder. Nonterminal batches remain provisional until native completion.
use super::{ReadError, ReadResult};
use crate::value::{Rec, Val};
use culprit_core::telemetry::CollectionStop;
use tokio::sync::mpsc;

const BUFFERED_BATCHES: usize = 1;

pub(crate) struct ReadBatch {
    pub rows: Vec<Rec>,
    pub complete: bool,
}
pub(super) enum Output {
    Buffered(Vec<Rec>),
    Streaming(Batches),
}
pub(super) struct Batches {
    sender: mpsc::Sender<ReadBatch>,
    rows: Vec<Rec>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    discarding: bool,
}
impl Output {
    pub fn buffered() -> Self {
        Self::Buffered(Vec::new())
    }
    pub fn streaming(max_rows: usize, max_bytes: usize) -> (Self, mpsc::Receiver<ReadBatch>) {
        let (sender, receiver) = mpsc::channel(BUFFERED_BATCHES);
        (Self::Streaming(Batches { sender, rows: vec![], bytes: 0, max_rows, max_bytes, discarding: false }), receiver)
    }
    pub async fn push(&mut self, row: Rec, bytes: usize) -> ReadResult<()> {
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
                    return Err(ReadError::Stopped(CollectionStop::ByteLimit));
                }
                if !batch.rows.is_empty()
                    && (batch.rows.len() >= batch.max_rows || batch.bytes.saturating_add(bytes) > batch.max_bytes)
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
    pub fn discarding(&self) -> bool {
        matches!(self,Self::Streaming(batch) if batch.discarding || batch.sender.is_closed())
    }
    pub async fn finish(self) -> Vec<Rec> {
        match self {
            Self::Buffered(rows) => rows,
            Self::Streaming(mut batch) => {
                batch.flush(true).await;
                vec![]
            }
        }
    }
}
impl Batches {
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
pub(super) fn value_bytes(row: &Rec) -> usize {
    row.columns().fold(0usize, |total, (_, value)| {
        total.saturating_add(match value {
            Val::Text(value) => value.len(),
            Val::Bytes(value) => value.len(),
            _ => 16,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(n: i64) -> Rec {
        Rec::of(&[("n", Val::Int(n))])
    }
    #[tokio::test]
    async fn the_final_batch_is_held_until_completion_and_buffered_delivery_keeps_the_same_rows() {
        let (mut streaming, mut receiver) = Output::streaming(2, 1024);
        let mut buffered = Output::buffered();
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
        let streamed: Vec<_> = first.rows.into_iter().chain(last.rows).map(|row| row.str("n").unwrap()).collect();
        let buffered: Vec<_> = buffered.finish().await.into_iter().map(|row| row.str("n").unwrap()).collect();
        assert_eq!(streamed, buffered);
    }
    #[tokio::test]
    async fn channel_backpressure_is_bounded_and_releasing_the_consumer_switches_to_discarded_delivery() {
        let (mut output, receiver) = Output::streaming(1, 1024);
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
        let (mut output, mut receiver) = Output::streaming(10, 16);
        output.push(row(1), 16).await.unwrap();
        output.push(row(2), 16).await.unwrap();
        let first = receiver.recv().await.unwrap();
        assert_eq!(first.rows.len(), 1);
        assert!(!first.complete);
        assert!(matches!(output.push(row(3), 17).await, Err(ReadError::Stopped(CollectionStop::ByteLimit))));
    }
}
