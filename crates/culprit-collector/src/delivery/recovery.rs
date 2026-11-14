//! A sink receives a committed capability only after the recovery provider acknowledges a fenced commit.
pub use super::diagnostic_recovery::{DiagnosticHead, DiagnosticHeadPage};
pub use super::object::RecoveryRecords;
use super::{contract::*, evidence::*};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{path::Path, pin::Pin, sync::Arc};
/// The recovery provider owns transport; common consumers bound and verify the stream at EOF.
pub type RecoveryReader = Pin<Box<dyn tokio::io::AsyncRead + Send>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadObject {
    pub object: String,
    pub sha256: String,
    pub bytes: u64,
}
impl From<&BatchManifest> for PayloadObject {
    fn from(batch: &BatchManifest) -> Self {
        Self { object: batch.object.clone(), sha256: batch.sha256.clone(), bytes: batch.bytes }
    }
}

/// Returned by the recovery provider. Implementations must fence every authoritative mutation against this epoch.
#[derive(Clone)]
pub struct Fence {
    epoch: u64,
    token: String,
    store_id: String,
}
impl Fence {
    pub fn issued(epoch: u64, token: String, store_id: String) -> Result<Self> {
        if epoch == 0 || !bounded_id(&token) || !valid_digest(&store_id) {
            return Err(DeliveryProblem::Integrity.error());
        }
        Ok(Self { epoch, token, store_id })
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn store_id(&self) -> &str {
        &self.store_id
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedBatch {
    pub store_id: String,
    pub object: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Clone)]
pub struct CommitReceipt {
    pub store_id: String,
    pub manifest_digest: String,
    pub head: String,
}
/// Commit order controls scheduling only; it never changes native source ordering or identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOrder {
    OldestFirst,
    NewestFirst,
}
#[derive(Clone)]
pub struct RecoveryPage {
    pub ranges: Vec<RangeManifest>,
    pub next_cursor: Option<String>,
    pub high_sequence: u64,
}

#[async_trait]
pub trait RecoveryStore: Send + Sync {
    fn id(&self) -> &str;
    async fn checkpoint_head(&self) -> Result<Option<String>>;
    /// A complete committed capture of this exact native range. Never inferred from a latest-N scan.
    async fn captured_range(
        &self,
        range: &culprit_core::telemetry::CapturedRange,
    ) -> Result<Option<culprit_core::telemetry::CaptureIdentity>>;
    async fn stage(&self, batch: &PayloadObject, path: &Path) -> Result<StagedBatch>;
    /// The implementation atomically checks the owner, previous head, immutable payload receipts and stream cursors.
    async fn commit(
        &self,
        fence: &Fence,
        expected_head: Option<&str>,
        manifest: &RangeManifest,
        staged: &[StagedBatch],
    ) -> Result<CommitReceipt>;
    async fn load(&self, range_id: &str) -> Result<RangeManifest>;
    /// Exact immutable capture lookup under this store's delivery contract; it does not query a native source.
    async fn capture_manifest(&self, capture: &culprit_core::telemetry::CaptureIdentity) -> Result<RangeManifest>;
    async fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage>;
    /// Cold recovery enumerates unpublished work, rather than replaying the entire retained history.
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32, order: RecoveryOrder) -> Result<RecoveryPage>;
    /// Native inputs still lacking their initial diagnostic checkpoint, including already-published captures.
    async fn pending_diagnostic_inputs(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage>;
    /// Exact current obligation membership for an authenticated native capture. Sequence maxima prove no absence.
    async fn diagnostic_input_pending(
        &self,
        input: &culprit_core::telemetry::DiagnosticCaptureRef,
        controller_version: u32,
    ) -> Result<bool>;
    async fn retire(&self, fence: &Fence, publication: &super::sink::PublishedRange) -> Result<()>;
    /// Latest full diagnostic working set per target and controller scope.
    async fn diagnostic_heads(&self, cursor: Option<&str>, limit: u32) -> Result<DiagnosticHeadPage>;
    /// Open the exact immutable object as a stream. Consumers verify its complete length and checksum; a partial
    /// HTTP response, truncated file or late read error cannot authorize publication.
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader>;
}
pub(crate) fn coverage_key(range: &culprit_core::telemetry::CapturedRange) -> Result<Option<String>> {
    if !valid_digest(&range.scope_id) {
        return Err(DeliveryProblem::Integrity.error());
    }
    if matches!(range.range, culprit_core::telemetry::TelemetryRange::Current) {
        return Ok(None);
    }
    let canonical = range.range.canonical()?;
    let bytes = serde_json::to_vec(&(&range.scope_id, canonical)).map_err(|_| DeliveryProblem::Integrity.error())?;
    if bytes.len() > culprit_core::telemetry::MAX_CURSOR_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok(Some(culprit_core::telemetry::digest(&bytes)))
}
/// Deliberately not deserializable: reopening a journal requires asking the recovery store for the committed manifest.
#[derive(Clone)]
pub struct CommittedRange {
    manifest: Arc<RangeManifest>,
    store_id: String,
    digest: String,
}
impl CommittedRange {
    pub fn manifest(&self) -> &RangeManifest {
        &self.manifest
    }
    pub fn store_id(&self) -> &str {
        &self.store_id
    }
    pub fn manifest_digest(&self) -> &str {
        &self.digest
    }
}
pub async fn stage_and_commit(
    store: &dyn RecoveryStore,
    fence: &Fence,
    expected_head: Option<&str>,
    prepared: &PreparedRange,
) -> Result<CommittedRange> {
    let manifest = prepared.manifest();
    manifest.validate()?;
    if store.id() != fence.store_id() {
        return Err(DeliveryProblem::Fenced.error());
    }
    let mut staged = Vec::with_capacity(manifest.batches.len());
    for batch in &manifest.batches {
        staged.push(stage_batch(store, batch, &prepared.batch_path(batch)).await?);
    }
    commit_staged(store, fence, expected_head, prepared.manifest.clone(), &staged).await
}
pub(crate) async fn stage_batch(store: &dyn RecoveryStore, batch: &BatchManifest, path: &Path) -> Result<StagedBatch> {
    let receipt = store.stage(&PayloadObject::from(batch), path).await?;
    check_staged(store.id(), batch, &receipt)?;
    Ok(receipt)
}
fn check_staged(store: &str, batch: &BatchManifest, receipt: &StagedBatch) -> Result<()> {
    if receipt.store_id != store
        || receipt.object != batch.object
        || receipt.sha256 != batch.sha256
        || receipt.bytes != batch.bytes
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(())
}
pub(crate) async fn commit_staged(
    store: &dyn RecoveryStore,
    fence: &Fence,
    expected_head: Option<&str>,
    manifest: Arc<RangeManifest>,
    staged: &[StagedBatch],
) -> Result<CommittedRange> {
    manifest.validate()?;
    if store.id() != fence.store_id() {
        return Err(DeliveryProblem::Fenced.error());
    }
    if staged.len() != manifest.batches.len() {
        return Err(DeliveryProblem::Integrity.error());
    }
    for (batch, receipt) in manifest.batches.iter().zip(staged) {
        check_staged(store.id(), batch, receipt)?;
    }
    let digest = manifest.digest()?;
    let receipt = store.commit(fence, expected_head, &manifest, staged).await?;
    if receipt.store_id != store.id() || receipt.manifest_digest != digest || receipt.head != manifest.range_id {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(CommittedRange { manifest, store_id: receipt.store_id, digest })
}
pub async fn reopen(store: &dyn RecoveryStore, range_id: &str) -> Result<CommittedRange> {
    let manifest = store.load(range_id).await?;
    manifest.validate()?;
    if manifest.range_id != range_id {
        return Err(DeliveryProblem::Integrity.error());
    }
    let digest = manifest.digest()?;
    Ok(CommittedRange { manifest: Arc::new(manifest), store_id: store.id().into(), digest })
}
