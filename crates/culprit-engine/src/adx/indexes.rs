//! Immutable expected projection digests, bounded to one native batch. Receipt paging never rereads its source blob.
use culprit_collector::delivery::{blob_store::transport::*, contract::MAX_RANGE_RECORDS, recovery::CommittedRange};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, valid_digest},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

#[cfg(test)]
mod tests;

const MAGIC: &[u8; 8] = b"CULPADX1";
const DIGEST_BYTES: usize = 32;
const HEADER_BYTES: usize = MAGIC.len() + DIGEST_BYTES;
const FOOTER_BYTES: usize = DIGEST_BYTES;
const ENTRY_BYTES: usize = DIGEST_BYTES + 4;
const MAX_ROW_RESPONSE_BYTES: u32 = 2 * 1024 * 1024 + 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedRow {
    pub digest: [u8; 32],
    /// Conservative bound for the verification-array encoding, including escaped dynamic values.
    pub response_bytes: u32,
}
impl ExpectedRow {
    pub fn new(digest: [u8; 32], wire_bytes: usize) -> Result<Self> {
        let response_bytes = wire_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(4096))
            .and_then(|bytes| u32::try_from(bytes).ok())
            .filter(|bytes| *bytes <= MAX_ROW_RESPONSE_BYTES)
            .ok_or_else(|| DeliveryProblem::Capacity.error())?;
        Ok(Self { digest, response_bytes })
    }
}

#[derive(Clone)]
pub struct IndexKey {
    object: String,
    identity: [u8; 32],
    rows: usize,
}
impl IndexKey {
    pub fn for_batch(range: &CommittedRange, batch_id: &str, schema_digest: &str) -> Result<Self> {
        let manifest = range.manifest();
        let batch = manifest.batches.iter().find(|batch| batch.batch_id == batch_id).ok_or_else(invalid)?;
        if !batch.delivery_required {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Self::new(
            &manifest.contract.id()?,
            &manifest.range_id,
            &batch.batch_id,
            &batch.sha256,
            schema_digest,
            batch.rows.len(),
        )
    }
    fn new(contract: &str, range: &str, batch: &str, native_sha: &str, schema: &str, rows: usize) -> Result<Self> {
        if rows > MAX_RANGE_RECORDS
            || [contract, range, batch, native_sha, schema].iter().any(|value| !valid_digest(value))
        {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&(contract, range, batch, native_sha, schema, rows)).map_err(|_| invalid())?;
        let identity = Sha256::digest(&bytes).into();
        Ok(Self {
            object: format!("validation/{contract}/{range}/{batch}/{schema}-{native_sha}.sha256"),
            identity,
            rows,
        })
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    fn bytes(&self) -> usize {
        HEADER_BYTES + self.rows * ENTRY_BYTES + FOOTER_BYTES
    }
}
struct CachedIndex {
    identity: [u8; 32],
    digests: Arc<[ExpectedRow]>,
}

pub struct ProjectionIndexes {
    transport: Arc<dyn BlobTransport>,
    /// One batch only. Verification consumes its pages sequentially; other ranges cannot grow a hidden cache.
    cached: tokio::sync::Mutex<Option<CachedIndex>>,
}
impl ProjectionIndexes {
    pub fn new(transport: Arc<dyn BlobTransport>) -> Self {
        Self { transport, cached: tokio::sync::Mutex::new(None) }
    }
    pub async fn put(&self, key: &IndexKey, digests: &[ExpectedRow]) -> Result<()> {
        if digests.len() != key.rows
            || digests.iter().any(|row| row.response_bytes == 0 || row.response_bytes > MAX_ROW_RESPONSE_BYTES)
        {
            return Err(invalid());
        }
        if key.rows == 0 {
            return Ok(());
        }
        let bytes = encode(key, digests);
        let expected = Sha256::digest(&bytes);
        if matches!(self.transport.put(&key.object, bytes, WriteCondition::Absent).await?, BlobWrite::ConditionNotMet) {
            let actual = self.read_bytes(key).await?;
            if Sha256::digest(&actual) != expected {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub async fn read(&self, key: &IndexKey) -> Result<Arc<[ExpectedRow]>> {
        if key.rows == 0 {
            return Ok(Arc::from([]));
        }
        let mut cached = self.cached.lock().await;
        if let Some(index) = cached.as_ref().filter(|index| index.identity == key.identity) {
            return Ok(index.digests.clone());
        }
        let bytes = self.read_bytes(key).await?;
        let digests: Arc<[ExpectedRow]> = decode(key, &bytes)?.into();
        *cached = Some(CachedIndex { identity: key.identity, digests: digests.clone() });
        Ok(digests)
    }
    async fn read_bytes(&self, key: &IndexKey) -> Result<Vec<u8>> {
        let mut object = self.transport.get(&key.object).await?.ok_or_else(|| DeliveryProblem::NotReady.error())?;
        if object.bytes != key.bytes() as u64 {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        (&mut object.body)
            .take(key.bytes() as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| DeliveryProblem::Transport.error())?;
        if bytes.len() != key.bytes() {
            return Err(invalid());
        }
        Ok(bytes)
    }
}
fn encode(key: &IndexKey, digests: &[ExpectedRow]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(key.bytes());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&key.identity);
    for digest in digests {
        bytes.extend_from_slice(&digest.digest);
        bytes.extend_from_slice(&digest.response_bytes.to_be_bytes());
    }
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}
fn decode(key: &IndexKey, bytes: &[u8]) -> Result<Vec<ExpectedRow>> {
    if bytes.len() != key.bytes() || &bytes[..MAGIC.len()] != MAGIC || bytes[MAGIC.len()..HEADER_BYTES] != key.identity
    {
        return Err(invalid());
    }
    let end = bytes.len() - FOOTER_BYTES;
    if Sha256::digest(&bytes[..end]).as_slice() != &bytes[end..] {
        return Err(invalid());
    }
    bytes[HEADER_BYTES..end]
        .as_chunks::<ENTRY_BYTES>()
        .0
        .iter()
        .map(|bytes| {
            let digest = bytes[..DIGEST_BYTES].try_into().map_err(|_| invalid())?;
            let response_bytes = u32::from_be_bytes(bytes[DIGEST_BYTES..].try_into().map_err(|_| invalid())?);
            if response_bytes == 0 || response_bytes > MAX_ROW_RESPONSE_BYTES {
                return Err(invalid());
            }
            Ok(ExpectedRow { digest, response_bytes })
        })
        .collect()
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
