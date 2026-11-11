use super::*;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};
struct Input {
    bytes: Vec<u8>,
    offset: usize,
    requested: Arc<AtomicUsize>,
    late_error: bool,
    stall: bool,
}
impl AsyncRead for Input {
    fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        self.requested.fetch_max(buffer.remaining(), Ordering::Relaxed);
        if self.stall {
            return Poll::Pending;
        }
        if self.offset == self.bytes.len() && self.late_error {
            return Poll::Ready(Err(std::io::Error::other("owned late read failure")));
        }
        let count = buffer.remaining().min(self.bytes.len() - self.offset);
        buffer.put_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Poll::Ready(Ok(()))
    }
}
fn reader(actual: Vec<u8>, expected: &[u8], late_error: bool, stall: bool) -> (VerifiedReader, Arc<AtomicUsize>) {
    let requested = Arc::new(AtomicUsize::new(0));
    let input = Input { bytes: actual, offset: 0, requested: requested.clone(), late_error, stall };
    (
        VerifiedReader {
            reader: Box::pin(input),
            expected_bytes: expected.len() as u64,
            expected_hash: culprit_core::telemetry::digest(expected),
            bytes: 0,
            hash: Sha256::new(),
            state: State::Reading,
        },
        requested,
    )
}
#[tokio::test]
async fn returned_data_is_chunk_bounded_and_only_complete_native_eof_verifies_the_object() {
    let bytes = vec![7; RECOVERY_CHUNK_BYTES * 3 + 17];
    let (mut input, requested) = reader(bytes.clone(), &bytes, false, false);
    let mut buffer = vec![0; RECOVERY_CHUNK_BYTES * 8];
    let mut read = 0;
    while {
        let count = input.read(&mut buffer).await.unwrap();
        read += count;
        count > 0
    } {}
    assert_eq!(read, bytes.len());
    assert_eq!(requested.load(Ordering::Relaxed), RECOVERY_CHUNK_BYTES);
    assert!(input.state == State::Verified);
}
#[tokio::test]
async fn same_length_corruption_truncation_extra_bytes_and_late_io_failure_never_verify() {
    for (actual, expected, late_error) in [
        (b"corrupt".to_vec(), b"correct".as_slice(), false),
        (b"short".to_vec(), b"correct".as_slice(), false),
        (b"correct+".to_vec(), b"correct".as_slice(), false),
        (b"correct".to_vec(), b"correct".as_slice(), true),
    ] {
        let (mut input, _) = reader(actual, expected, late_error, false);
        let mut buffer = [0; 32];
        loop {
            match input.read(&mut buffer).await {
                Ok(0) => panic!("invalid source must not verify"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(input.state == State::Failed);
        assert!(input.read(&mut buffer).await.is_err());
    }
}
#[tokio::test]
async fn retiring_a_recovery_read_cannot_resume_as_if_no_bytes_were_lost() {
    let (mut input, _) = reader(vec![], b"", false, true);
    let mut buffer = [0; 4];
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), input.read(&mut buffer)).await.is_err());
    assert!(input.read(&mut buffer).await.is_err());
    assert!(input.state == State::Failed);
}
#[tokio::test]
async fn oversized_record_is_rejected_before_its_line_buffer_exceeds_policy() {
    let bytes = vec![b'x'; MAX_RECORD_BYTES + 1];
    let (reader, _) = reader(bytes.clone(), &bytes, false, false);
    let mut records =
        RecoveryRecords { reader, buffer: vec![0; RECOVERY_CHUNK_BYTES], offset: 0, end: 0, line: vec![] };
    assert!(records.next().await.is_err());
    assert!(records.line.len() <= MAX_RECORD_BYTES);
}
