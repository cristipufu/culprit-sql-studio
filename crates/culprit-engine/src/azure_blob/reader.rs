//! A bounded blocking-HTTP bridge. Only native EOF produces successful AsyncRead EOF.
use super::*;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::mpsc,
};

pub(super) type Chunk = io::Result<Vec<u8>>;
pub(super) struct DownloadReader {
    receiver: mpsc::Receiver<Chunk>,
    chunk: Vec<u8>,
    offset: usize,
    finished: bool,
}
impl DownloadReader {
    pub fn new(receiver: mpsc::Receiver<Chunk>) -> Self {
        Self { receiver, chunk: vec![], offset: 0, finished: false }
    }
}
impl AsyncRead for DownloadReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, output: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if output.remaining() == 0 || self.finished {
            return Poll::Ready(Ok(()));
        }
        if self.offset == self.chunk.len() {
            match self.receiver.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) if chunk.is_empty() => {
                    self.finished = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    self.chunk = chunk;
                    self.offset = 0;
                }
                Poll::Ready(Some(Err(error))) => {
                    self.finished = true;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof)));
                }
            }
        }
        let count = output.remaining().min(self.chunk.len() - self.offset);
        output.put_slice(&self.chunk[self.offset..self.offset + count]);
        self.offset += count;
        Poll::Ready(Ok(()))
    }
}
pub(super) fn stream(mut body: impl io::Read, bytes: u64, sender: mpsc::Sender<Chunk>) {
    let mut total = 0u64;
    loop {
        let mut chunk = vec![0; DOWNLOAD_CHUNK_BYTES];
        let result = body.read(&mut chunk);
        let count = match result {
            Ok(count) => count,
            Err(_) => {
                let _ = sender.blocking_send(Err(io::Error::from(io::ErrorKind::ConnectionReset)));
                return;
            }
        };
        total += count as u64;
        if total > bytes || (count == 0 && total != bytes) {
            let _ = sender.blocking_send(Err(io::Error::from(io::ErrorKind::UnexpectedEof)));
            return;
        }
        chunk.truncate(count);
        if sender.blocking_send(Ok(chunk)).is_err() || count == 0 {
            return;
        }
    }
}
