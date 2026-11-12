//! Recovery over immutable objects and one conditional checkpoint. No listing consistency is assumed.
//! The root is the commit authority. Indexes are written afterwards; the next commit must repair them first.
//! An ownership token fences delivery writes only. It does not prove a previous SQL request has stopped.
pub mod bridge;
pub mod provider;
mod recovery;
mod state;
#[cfg(test)]
mod tests;
pub mod transport;

use super::{contract::*, evidence::*, recovery::*};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest},
};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use state::*;
use std::{path::Path, sync::Arc};
use tokio::io::AsyncReadExt;
use transport::*;

fn integrity() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
fn transport_error() -> culprit_core::Error {
    DeliveryProblem::Transport.error()
}

pub struct ObjectRecoveryStore {
    id: String,
    contract: Contract,
    transport: Arc<dyn ObjectTransport>,
}
impl ObjectRecoveryStore {
    /// Explicit provisioning. Opening a missing namespace never silently creates a replacement history.
    pub async fn create(transport: Arc<dyn ObjectTransport>, contract: Contract) -> Result<Self> {
        let root = Root::new(contract.clone())?;
        let bytes = encode(&root, MAX_ROOT_BYTES)?;
        if !matches!(transport.put(ROOT_KEY, bytes, WriteCondition::Absent).await?, ObjectWrite::Written) {
            return Err(DeliveryProblem::Conflict.error());
        }
        Ok(Self { id: root.store_id, contract, transport })
    }
    pub async fn open(transport: Arc<dyn ObjectTransport>, contract: Contract) -> Result<Self> {
        contract.validate()?;
        let (root, _) = read_json::<Root>(&*transport, ROOT_KEY, MAX_ROOT_BYTES)
            .await?
            .ok_or_else(|| DeliveryProblem::NotReady.error())?;
        root.validate(&contract)?;
        Ok(Self { id: root.store_id, contract, transport })
    }
    async fn root(&self) -> Result<(Root, String)> {
        let (root, etag) =
            read_json::<Root>(&*self.transport, ROOT_KEY, MAX_ROOT_BYTES).await?.ok_or_else(integrity)?;
        root.validate(&self.contract)?;
        if root.store_id != self.id {
            return Err(integrity());
        }
        Ok((root, etag))
    }
    async fn replace_root(&self, root: &Root, etag: &str) -> Result<()> {
        root.validate(&self.contract)?;
        let bytes = encode(root, MAX_ROOT_BYTES)?;
        match self.transport.put(ROOT_KEY, bytes, WriteCondition::Matches(etag)).await? {
            ObjectWrite::Written => Ok(()),
            ObjectWrite::ConditionNotMet => Err(DeliveryProblem::Conflict.error()),
        }
    }
    /// Call once during explicit owner startup. There is no lease-expiry takeover or automatic reacquisition.
    pub async fn acquire(&self) -> Result<Fence> {
        let (mut root, etag) = self.root().await?;
        root.epoch = root.epoch.checked_add(1).ok_or_else(integrity)?;
        let token = crate::durable_fs::random_id()?;
        root.token = Some(token.clone());
        self.replace_root(&root, &etag).await?;
        Fence::issued(root.epoch, token, self.id.clone())
    }
    pub async fn checkpoint(&self, key: &str) -> Result<Option<String>> {
        Ok(self.root().await?.0.checkpoints.get(key).cloned())
    }
    async fn immutable(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        let expected = digest(&bytes);
        let count = bytes.len();
        match self.transport.put(key, bytes, WriteCondition::Absent).await? {
            ObjectWrite::Written => Ok(()),
            ObjectWrite::ConditionNotMet => {
                let value = self.transport.get(key).await?.ok_or_else(integrity)?;
                let existing = read_bytes(value, count).await?;
                if existing.len() != count || digest(&existing) != expected {
                    return Err(integrity());
                }
                Ok(())
            }
        }
    }
    async fn verify_payload(&self, batch: &PayloadObject) -> Result<()> {
        validate_payload(batch)?;
        let value = self.transport.get(&batch.object).await?.ok_or_else(integrity)?;
        if value.bytes != batch.bytes {
            return Err(integrity());
        }
        let mut reader = value.body;
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut buffer = vec![0u8; super::object::RECOVERY_CHUNK_BYTES];
        loop {
            let count = reader.read(&mut buffer).await.map_err(|_| transport_error())?;
            if count == 0 {
                break;
            }
            total = total.checked_add(count as u64).filter(|total| *total <= batch.bytes).ok_or_else(integrity)?;
            hash.update(&buffer[..count]);
        }
        let hash = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        if total != batch.bytes || hash != batch.sha256 {
            return Err(integrity());
        }
        Ok(())
    }
    async fn repair_indexes(&self, root: &Root) -> Result<()> {
        let Some(head) = &root.head else {
            return Ok(());
        };
        if root.indexed_order == head.order {
            return Ok(());
        }
        let bytes = encode(head, MAX_INDEX_BYTES)?;
        self.immutable(&head.range_key(), bytes.clone()).await?;
        self.immutable(&head.order_key(), bytes.clone()).await?;
        let manifest = self.manifest(head).await?;
        let range = manifest.source.captured_range()?;
        let Some(key) = coverage_key(&range)? else {
            return Ok(());
        };
        let key = format!("coverage/{key}.json");
        match self.transport.put(&key, bytes, WriteCondition::Absent).await? {
            ObjectWrite::Written => Ok(()),
            ObjectWrite::ConditionNotMet => {
                let existing = self.index(&key, root).await?.ok_or_else(integrity)?;
                self.capture_for_index(&existing, &range, root).await.map(|_| ())
            }
        }
    }
    async fn index(&self, key: &str, root: &Root) -> Result<Option<CommitIndex>> {
        let index = read_json::<CommitIndex>(&*self.transport, key, MAX_INDEX_BYTES).await?.map(|(index, _)| index);
        if let Some(index) = &index {
            index.validate(&self.id)?;
            if root.head.as_ref().is_none_or(|head| index.order > head.order) {
                return Err(integrity());
            }
        }
        Ok(index)
    }
    async fn manifest(&self, index: &CommitIndex) -> Result<RangeManifest> {
        let value = self.transport.get(&index.manifest_key()).await?.ok_or_else(integrity)?;
        let bytes = read_bytes(value, MAX_MANIFEST_BYTES).await?;
        if digest(&bytes) != index.manifest_digest {
            return Err(integrity());
        }
        let manifest: RangeManifest = serde_json::from_slice(&bytes).map_err(|_| integrity())?;
        manifest.validate()?;
        if manifest.contract != self.contract || manifest.range_id != index.range_id {
            return Err(integrity());
        }
        Ok(manifest)
    }
}
fn validate_payload(batch: &PayloadObject) -> Result<()> {
    if !valid_digest(&batch.sha256)
        || batch.object != format!("payloads/{}.ndjson", batch.sha256)
        || batch.bytes > MAX_RANGE_BYTES
    {
        return Err(integrity());
    }
    Ok(())
}
fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| integrity())?;
    if bytes.len() > limit {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok(bytes)
}
async fn read_json<T: DeserializeOwned>(
    transport: &dyn ObjectTransport,
    key: &str,
    limit: usize,
) -> Result<Option<(T, String)>> {
    let Some(value) = transport.get(key).await? else {
        return Ok(None);
    };
    if value.etag.is_empty() || value.etag.len() > 256 || value.etag.bytes().any(|b| b.is_ascii_control()) {
        return Err(integrity());
    }
    let etag = value.etag.clone();
    let bytes = read_bytes(value, limit).await?;
    let parsed = serde_json::from_slice(&bytes).map_err(|_| integrity())?;
    Ok(Some((parsed, etag)))
}
async fn read_bytes(mut value: ObjectRead, limit: usize) -> Result<Vec<u8>> {
    if value.bytes > limit as u64 {
        return Err(DeliveryProblem::Capacity.error());
    }
    let mut bytes = Vec::new();
    (&mut value.body).take(value.bytes + 1).read_to_end(&mut bytes).await.map_err(|_| transport_error())?;
    if bytes.len() as u64 != value.bytes {
        return Err(integrity());
    }
    Ok(bytes)
}

#[cfg(feature = "test-support")]
pub mod test_support;
