//! Bounded recovery reads verify size and hash at native EOF before a consumer can publish any receipts.
use super::{
    contract::*,
    evidence::BatchManifest,
    recovery::{PayloadObject, RecoveryReader, RecoveryStore},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, ExportedRecord},
};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const RECOVERY_CHUNK_BYTES: usize = 64 * 1024;

struct VerifiedReader {
    reader: RecoveryReader,
    expected_bytes: u64,
    expected_hash: String,
    bytes: u64,
    hash: Sha256,
    state: State,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Reading,
    Verified,
    Failed,
}
impl VerifiedReader {
    async fn open(store: &dyn RecoveryStore, batch: &BatchManifest) -> Result<Self> {
        if batch.bytes > MAX_RANGE_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        if !valid_digest(&batch.sha256) {
            return Err(DeliveryProblem::Integrity.error());
        }
        Ok(Self {
            reader: store.open_object(&PayloadObject::from(batch)).await?,
            expected_bytes: batch.bytes,
            expected_hash: batch.sha256.clone(),
            bytes: 0,
            hash: Sha256::new(),
            state: State::Reading,
        })
    }
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if self.state == State::Verified {
            return Ok(0);
        }
        if self.state != State::Reading || buffer.is_empty() {
            return Err(DeliveryProblem::Integrity.error());
        }
        self.state = State::Failed;
        let limit = buffer.len().min(RECOVERY_CHUNK_BYTES);
        let count = self.reader.read(&mut buffer[..limit]).await.map_err(|_| DeliveryProblem::Storage.error())?;
        self.bytes = self.bytes.checked_add(count as u64).ok_or_else(|| DeliveryProblem::Capacity.error())?;
        if self.bytes > self.expected_bytes {
            return Err(DeliveryProblem::Integrity.error());
        }
        self.hash.update(&buffer[..count]);
        if count == 0 {
            let hash = self.hash.clone().finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            if self.bytes != self.expected_bytes || hash != self.expected_hash {
                return Err(DeliveryProblem::Integrity.error());
            }
            self.state = State::Verified;
        } else {
            self.state = State::Reading;
        }
        Ok(count)
    }
}

/// One record is retained at a time. The final None requires complete byte and checksum verification.
pub struct RecoveryRecords {
    reader: VerifiedReader,
    buffer: Vec<u8>,
    offset: usize,
    end: usize,
    line: Vec<u8>,
}
impl RecoveryRecords {
    pub async fn open(store: &dyn RecoveryStore, batch: &BatchManifest) -> Result<Self> {
        Ok(Self {
            reader: VerifiedReader::open(store, batch).await?,
            buffer: vec![0; RECOVERY_CHUNK_BYTES],
            offset: 0,
            end: 0,
            line: Vec::new(),
        })
    }
    pub async fn next(&mut self) -> Result<Option<ExportedRecord>> {
        loop {
            if self.offset == self.end {
                self.end = self.reader.read(&mut self.buffer).await?;
                self.offset = 0;
                if self.end == 0 {
                    return if self.line.is_empty() { Ok(None) } else { self.decode().map(Some) };
                }
            }
            let available = &self.buffer[self.offset..self.end];
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if count > MAX_RECORD_BYTES.saturating_sub(self.line.len()) {
                return Err(DeliveryProblem::Capacity.error());
            }
            self.line.extend_from_slice(&available[..count]);
            self.offset += count;
            if newline.is_some() {
                self.offset += 1;
                return self.decode().map(Some);
            }
        }
    }
    fn decode(&mut self) -> Result<ExportedRecord> {
        let row = serde_json::from_slice(&self.line).map_err(|_| DeliveryProblem::Integrity.error())?;
        self.line.clear();
        Ok(row)
    }
}

#[cfg(test)]
mod tests;
