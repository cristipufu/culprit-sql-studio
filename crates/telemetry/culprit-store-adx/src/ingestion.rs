//! Both ADX transports consume the same immutable analytical batches and completion records.
use async_trait::async_trait;
use culprit_collector::{
    analytical::encoding::EncodedBatch,
    delivery::{
        contract::MAX_RANGE_BYTES, object_store::transport::valid_object_key, recovery::CommittedRange,
        sink::DeliveryAttempt,
    },
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest, valid_digest},
};
use std::{
    io::{Cursor, Read},
    sync::Arc,
};

#[async_trait]
pub trait IngestionTransport: Send + Sync {
    /// Acceptance is a transport acknowledgement. Exact readback still authorizes publication.
    async fn submit(&self, batch: Arc<IngestionBatch>) -> Result<()>;
}

pub struct IngestionBatch {
    table: String,
    key: String,
    bytes: u64,
    sha256: String,
    payload: Payload,
}
pub(crate) enum Payload {
    Encoded(Arc<EncodedBatch>),
    Publication(Arc<[u8]>),
}
impl IngestionBatch {
    pub fn encoded(range: &CommittedRange, attempt: &DeliveryAttempt, batch: Arc<EncodedBatch>) -> Result<Self> {
        Self::new(&batch.table, range, &batch.batch_id, attempt, Payload::Encoded(batch.clone()))
    }
    pub fn publication(
        table: &str,
        range: &CommittedRange,
        id: &str,
        attempt: &DeliveryAttempt,
        bytes: Vec<u8>,
    ) -> Result<Self> {
        Self::new(table, range, id, attempt, Payload::Publication(bytes.into()))
    }
    fn new(
        table: &str,
        range: &CommittedRange,
        batch: &str,
        attempt: &DeliveryAttempt,
        payload: Payload,
    ) -> Result<Self> {
        attempt.validate()?;
        let (bytes, sha256) = match &payload {
            Payload::Encoded(batch) => (batch.bytes, batch.sha256.clone()),
            Payload::Publication(bytes) => (bytes.len() as u64, digest(bytes)),
        };
        if table.is_empty()
            || !table.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !valid_digest(batch)
            || !valid_digest(&sha256)
            || bytes > MAX_RANGE_BYTES
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let key = format!("ready/{table}/{}/{batch}/{}-{sha256}.ndjson", range.manifest().range_id, attempt.id);
        if !valid_object_key(&key) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(Self { table: table.into(), key, bytes, sha256, payload })
    }
    pub fn table(&self) -> &str {
        &self.table
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub(crate) fn payload(&self) -> &Payload {
        &self.payload
    }
    pub(crate) fn reader(&self) -> Result<Box<dyn Read + Send>> {
        match &self.payload {
            Payload::Encoded(batch) => {
                let file = std::fs::File::open(batch.path()).map_err(|_| DeliveryProblem::Storage.error())?;
                let metadata = file.metadata().map_err(|_| DeliveryProblem::Storage.error())?;
                if !metadata.is_file() || metadata.len() != self.bytes {
                    return Err(DeliveryProblem::Integrity.error());
                }
                Ok(Box::new(file))
            }
            Payload::Publication(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
        }
    }
}
