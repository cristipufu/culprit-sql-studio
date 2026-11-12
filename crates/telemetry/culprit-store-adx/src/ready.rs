//! Fresh ready-prefix objects are created only from completely verified, committed native batches.
use crate::ingestion::{IngestionBatch, IngestionTransport, Payload};
use async_trait::async_trait;
use culprit_collector::delivery::object_store::transport::*;
use culprit_core::{Result, telemetry::DeliveryProblem};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

pub struct ReadyDispatcher {
    ready: Arc<dyn ObjectTransport>,
}
impl ReadyDispatcher {
    /// The Event Grid subscription must cover only ready/<table>/, never native staging or validation objects.
    pub fn new(ready: Arc<dyn ObjectTransport>) -> Self {
        Self { ready }
    }
}
#[async_trait]
impl IngestionTransport for ReadyDispatcher {
    async fn submit(&self, batch: Arc<IngestionBatch>) -> Result<()> {
        if batch.bytes() == 0 {
            return Ok(());
        }
        let ready = self.ready.clone();
        // The batch lease survives caller cancellation until the upload has settled.
        tokio::spawn(async move {
            let written = match batch.payload() {
                Payload::Encoded(encoded) => ready.put_file(batch.key(), encoded.path(), batch.bytes()).await?,
                Payload::Publication(bytes) => ready.put(batch.key(), bytes.to_vec(), WriteCondition::Absent).await?,
            };
            if matches!(written, ObjectWrite::ConditionNotMet) {
                verify_existing(ready.as_ref(), batch.key(), batch.bytes(), batch.sha256()).await?;
            }
            Ok(())
        })
        .await
        .map_err(|_| DeliveryProblem::Transport.error())?
    }
}
async fn verify_existing(
    transport: &dyn ObjectTransport,
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
