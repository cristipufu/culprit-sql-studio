//! Fresh ready-prefix objects are created only from completely verified, committed native batches.
use super::{encoding::BatchEncoder, indexes::ProjectionIndexes};
use culprit_collector::delivery::{
    blob_store::transport::*,
    recovery::CommittedRange,
    sink::{DeliveryAttempt, Submission},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

pub struct ReadyDispatcher {
    encoder: BatchEncoder,
    indexes: Arc<ProjectionIndexes>,
    ready: Arc<dyn BlobTransport>,
}
impl ReadyDispatcher {
    /// The Event Grid subscription must cover only ready/<table>/, never native staging or validation objects.
    pub fn new(encoder: BatchEncoder, indexes: Arc<ProjectionIndexes>, ready: Arc<dyn BlobTransport>) -> Self {
        Self { encoder, indexes, ready }
    }
    pub async fn dispatch(
        &self,
        range: &CommittedRange,
        attempt: &DeliveryAttempt,
        batch_id: &str,
    ) -> Result<Submission> {
        validate_attempt(attempt)?;
        let encoded = self.encoder.encode(range, batch_id).await?;
        self.indexes.put(&encoded.index_key, &encoded.digests).await?;
        if encoded.bytes == 0 {
            return Ok(Submission::Accepted);
        }
        let key = object_key(&encoded.table, range, batch_id, attempt, &encoded.sha256)?;
        if matches!(self.ready.put_file(&key, encoded.path(), encoded.bytes).await?, BlobWrite::ConditionNotMet) {
            verify_existing(self.ready.as_ref(), &key, encoded.bytes, &encoded.sha256).await?;
        }
        // Blob acknowledgement does not establish ADX ingestion. Exact analytical receipts authorize publication.
        Ok(Submission::Accepted)
    }
}
pub(super) async fn publish_bytes(
    transport: &dyn BlobTransport,
    table: &str,
    range: &CommittedRange,
    batch: &str,
    attempt: &DeliveryAttempt,
    bytes: Vec<u8>,
) -> Result<()> {
    let sha256 = culprit_core::telemetry::digest(&bytes);
    let count = bytes.len() as u64;
    let key = object_key(table, range, batch, attempt, &sha256)?;
    if matches!(transport.put(&key, bytes, WriteCondition::Absent).await?, BlobWrite::ConditionNotMet) {
        verify_existing(transport, &key, count, &sha256).await?;
    }
    Ok(())
}
fn object_key(
    table: &str,
    range: &CommittedRange,
    batch: &str,
    attempt: &DeliveryAttempt,
    sha256: &str,
) -> Result<String> {
    validate_attempt(attempt)?;
    Ok(format!("ready/{table}/{}/{batch}/{}-{sha256}.ndjson", range.manifest().range_id, attempt.id))
}
fn validate_attempt(attempt: &DeliveryAttempt) -> Result<()> {
    if attempt.ordinal == 0 || attempt.id.len() != 32 || !attempt.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(())
}
async fn verify_existing(
    transport: &dyn BlobTransport,
    key: &str,
    expected_bytes: u64,
    expected_sha: &str,
) -> Result<()> {
    let mut object = transport.get(key).await?.ok_or_else(invalid)?;
    if object.bytes != expected_bytes {
        return Err(invalid());
    }
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    loop {
        let count = object.body.read(&mut buffer).await.map_err(|_| DeliveryProblem::Transport.error())?;
        bytes = bytes.checked_add(count as u64).filter(|bytes| *bytes <= expected_bytes).ok_or_else(invalid)?;
        hash.update(&buffer[..count]);
        if count == 0 {
            break;
        }
    }
    let actual = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if bytes != expected_bytes || actual != expected_sha {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
