//! Jobs-specific original capture composition over the production Outbox and existing native factory.
use super::*;
use culprit_collector::capture::collect;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const MAX_MANIFEST_BYTES: usize = 128 * 1024;
pub(super) struct Original {
    pub manifest: Box<CollectionManifest>,
    pub error: Option<culprit_core::ErrorCode>,
    output: PathBuf,
    manifest_sha256: String,
}
pub(super) async fn take(
    factory: &Factory,
    outbox: &mut Outbox,
    gate: &SourceGate,
    cancellation: &CollectionCancellation,
) -> Result<Original> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let request = request();
    let mut lease = outbox.reserve("owned-scheduled-event-catalog", &request).await?;
    let identity = lease.identity().clone();
    let output = lease.directory().to_owned();
    let result =
        Box::pin(collect(gate, factory, request, &output, identity.clone(), cancellation, Some(&mut lease))).await;
    let error = result.as_ref().err().map(|error| error.code);
    let shutdown = factory.0.source_shutdown();
    let commit = lease.finish().await;
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    commit?;
    if let Err(error) = result {
        if error.code != culprit_core::ErrorCode::PermissionDenied || error.detail.is_some() {
            return Err(error);
        }
        // Retain the actual failed manifest; a native access denial is never converted to empty success.
    }
    let bytes = read_bounded(&output.join("manifest.json"), MAX_MANIFEST_BYTES)?;
    let original: Box<CollectionManifest> = serde_json::from_slice(&bytes).map_err(|_| native::invalid())?;
    if original.capture != identity || identity.revision.is_none() {
        return Err(native::invalid());
    }
    let manifest = if error.is_none() {
        if original.status != CollectionStatus::Complete {
            return Err(native::invalid());
        }
        Box::new(outbox.verify(&identity.observation_id).await?)
    } else {
        original
    };
    let metrics = manifest.read_metrics.as_ref().ok_or_else(native::invalid)?;
    if !metrics.is_valid()
        || metrics.has_unconfirmed_work()
        || metrics.active_tasks != 0
        || metrics.untracked_tasks != 0
        || metrics.templates.iter().map(|query| query.completed_tasks).sum::<u32>() != manifest.source_queries
        || manifest.source_queries > SOURCE_QUERIES
    {
        return Err(native::invalid());
    }
    Ok(Original { manifest, error, output, manifest_sha256: digest(&bytes) })
}
impl Original {
    pub(super) fn require_root_rows(&self, rows: u32) -> Result<()> {
        let source = self.manifest.description.as_ref().ok_or_else(native::invalid)?;
        let SourceObservation::MysqlEvents { snapshot, metadata } = &source.observation else {
            return Err(native::invalid());
        };
        if self.error.is_some()
            || self.manifest.status != CollectionStatus::Complete
            || snapshot.records != rows
            || !metadata.matches_source(source, snapshot)
            || !metadata.database_event_visibility
            || metadata.layout != MysqlEventCatalogLayout::Available
        {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) async fn verify(&self, outbox: &mut Outbox) -> Result<()> {
        self.verify_original()?;
        let verified = outbox.verify(&self.manifest.capture.observation_id).await?;
        if serde_json::to_value(verified).map_err(|_| native::invalid())?
            != serde_json::to_value(&self.manifest).map_err(|_| native::invalid())?
        {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) fn verify_original(&self) -> Result<()> {
        if digest(&read_bounded(&self.output.join("manifest.json"), MAX_MANIFEST_BYTES)?) != self.manifest_sha256 {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) fn replay(&self) -> capture::Capture {
        capture::read_capture(self.manifest.as_ref().clone(), &request().budget, &self.output)
    }
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| native::invalid())?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| native::invalid())?;
    (bytes.len() <= limit).then_some(bytes).ok_or_else(native::invalid)
}
