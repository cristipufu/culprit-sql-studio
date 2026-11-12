//! Bounded ownership bridge for blocking object transports. Native EOF is required before successful async EOF.
use super::transport::ObjectRead;
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{Semaphore, mpsc, oneshot},
};

pub const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
pub type Chunk = io::Result<Vec<u8>>;
pub struct Download {
    pub etag: String,
    pub bytes: u64,
    pub body: Box<dyn io::Read + Send>,
}
pub async fn download(
    requests: Arc<Semaphore>,
    read: impl FnOnce() -> Result<Option<Download>> + Send + 'static,
) -> Result<Option<ObjectRead>> {
    let permit = requests.acquire_owned().await.map_err(|_| transport())?;
    let (metadata, reply) = oneshot::channel();
    let (chunks, receiver) = mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match read() {
            Ok(Some(download)) => {
                if metadata.send(Ok(Some((download.etag, download.bytes)))).is_ok() {
                    stream(download.body, download.bytes, chunks);
                }
            }
            Ok(None) => {
                let _ = metadata.send(Ok(None));
            }
            Err(error) => {
                let _ = metadata.send(Err(error));
            }
        }
    });
    let Some((etag, bytes)) = reply.await.map_err(|_| transport())?? else { return Ok(None) };
    Ok(Some(ObjectRead { etag, bytes, body: Box::pin(DownloadReader::new(receiver)) }))
}
pub struct DownloadReader {
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
                    return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                }
            }
        }
        let count = output.remaining().min(self.chunk.len() - self.offset);
        output.put_slice(&self.chunk[self.offset..self.offset + count]);
        self.offset += count;
        Poll::Ready(Ok(()))
    }
}
pub fn stream(mut body: impl io::Read, bytes: u64, sender: mpsc::Sender<Chunk>) {
    let mut total = 0u64;
    loop {
        let mut chunk = vec![0; DOWNLOAD_CHUNK_BYTES];
        let count = match body.read(&mut chunk) {
            Ok(count) => count,
            Err(_) => {
                let _ = sender.blocking_send(Err(io::ErrorKind::ConnectionReset.into()));
                return;
            }
        };
        total += count as u64;
        if total > bytes || (count == 0 && total != bytes) {
            let _ = sender.blocking_send(Err(io::ErrorKind::UnexpectedEof.into()));
            return;
        }
        chunk.truncate(count);
        if sender.blocking_send(Ok(chunk)).is_err() || count == 0 {
            return;
        }
    }
}
fn transport() -> culprit_core::Error {
    DeliveryProblem::Transport.error()
}

/// Pin an exact regular file before the first asynchronous yield; a cancelled caller may remove its pathname.
pub fn open_upload(path: &std::path::Path, bytes: u64) -> Result<std::fs::File> {
    if !std::fs::symlink_metadata(path).map_err(|_| transport())?.is_file() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let file = std::fs::File::open(path).map_err(|_| transport())?;
    let metadata = file.metadata().map_err(|_| transport())?;
    if !metadata.is_file() || metadata.len() != bytes {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn lost_native_worker_is_an_error_instead_of_successful_eof() {
        let (sender, receiver) = mpsc::channel(1);
        sender.send(Ok(b"prefix".to_vec())).await.unwrap();
        drop(sender);
        let mut reader = DownloadReader::new(receiver);
        let error = reader.read_to_end(&mut vec![]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
    struct Blocked(std::sync::mpsc::Receiver<()>);
    impl io::Read for Blocked {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.0.recv_timeout(std::time::Duration::from_secs(5)).map_err(io::Error::other)?;
            Ok(0)
        }
    }
    #[tokio::test]
    async fn dropping_a_download_keeps_native_admission_owned_until_the_worker_finishes() {
        let gate = Arc::new(Semaphore::new(1));
        let (release, blocked) = std::sync::mpsc::channel();
        let object = download(gate.clone(), move || {
            Ok(Some(Download { etag: "one".into(), bytes: 0, body: Box::new(Blocked(blocked)) }))
        })
        .await
        .unwrap()
        .unwrap();
        drop(object);
        assert!(gate.try_acquire().is_err(), "an abandoned native read still owns its permit");
        release.send(()).unwrap();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), gate.acquire()).await.unwrap().unwrap();
        drop(permit);
    }
}
