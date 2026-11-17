//! One fixture-owned outbox retains the actual original parents through selected native followups. Collection,
//! source cancellation, publication and lease commit use the production collector; there is no second reader.
use super::fixture::{Fixture, invalid};
use crate::conn::Pool;
use culprit_collector::{
    capture::{CollectionCancellation, SourceFactory, collect},
    outbox::Outbox,
};
use culprit_core::{Result, telemetry::*};
use culprit_read::admission::SourceGate;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

const MAX_MANIFEST_BYTES: usize = 128 * 1024;

struct Factory {
    pool: Arc<Pool>,
    port: u16,
}
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        crate::telemetry::source(self.pool.clone(), request, "127.0.0.1".into(), self.port)
    }
}

pub(super) struct OwnedCaptures {
    outbox: Outbox,
    _directory: tempfile::TempDir,
    factory: Factory,
    gate: SourceGate,
}

pub(super) struct Captured {
    pub manifest: CollectionManifest,
    pub records: Vec<ExportedRecord>,
    manifest_sha256: String,
    manifest_path: PathBuf,
    /// Whole capture/export time is separate from native reader client accounting, never database CPU/I/O.
    pub capture_export_ms: f64,
}

impl OwnedCaptures {
    pub fn new(fixture: &Fixture) -> Result<Self> {
        let directory = tempfile::tempdir().map_err(|_| invalid("owned artifact directory could not be created"))?;
        let outbox = Outbox::create(&directory.path().join("outbox"), OutboxLimits::default())?;
        Ok(Self {
            outbox,
            _directory: directory,
            factory: Factory {
                pool: fixture.pool.as_ref().ok_or_else(|| invalid("owned fixture pool absent"))?.clone(),
                port: fixture.port,
            },
            gate: SourceGate::default(),
        })
    }

    pub async fn capture(&mut self, selection: CollectionSelection) -> Result<Captured> {
        self.capture_with_cancellation(selection, &CollectionCancellation::default()).await
    }

    pub async fn capture_with_cancellation(
        &mut self,
        selection: CollectionSelection,
        cancellation: &CollectionCancellation,
    ) -> Result<Captured> {
        let provisional = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: selection.clone(),
            budget: CollectionBudget::default(),
        };
        let recipe = crate::telemetry::recipes()
            .into_iter()
            .find(|recipe| recipe.selector.matches(&provisional))
            .ok_or_else(|| invalid("native fixture recipe not declared"))?;
        let request = CollectionRequest { budget: provisional.budget.intersection(&recipe.budget), ..provisional };
        let started = Instant::now();
        let mut lease = self.outbox.reserve("owned-page-holder-fixture", &request).await?;
        let identity = lease.identity().clone();
        let output = lease.directory().to_owned();
        let manifest =
            collect(&self.gate, &self.factory, request, &output, identity.clone(), cancellation, Some(&mut lease))
                .await?;
        if manifest.capture != identity || identity.revision.is_none() {
            return Err(invalid("native parent did not retain its actual durable lease"));
        }
        if let CollectionStatus::Incomplete(stop) = manifest.status {
            // The capture owner has already settled or declared its native debt. Persist that exact incomplete
            // artifact/status before returning; cancellation never becomes a complete or quiescent parent.
            lease.finish().await?;
            return Err(stop.error());
        }
        let metrics = manifest.read_metrics.as_ref().ok_or_else(|| invalid("native source accounting absent"))?;
        if !metrics.is_valid()
            || metrics.has_unconfirmed_work()
            || metrics.untracked_tasks != 0
            || metrics.templates.iter().map(|task| task.completed_tasks).sum::<u32>() != manifest.source_queries
        {
            return Err(invalid("native source work was not completely accounted and settled"));
        }
        lease.finish().await?;
        // The immutable original is the verified persisted representation. Native elapsed-time floats can
        // change their last bit during JSON decoding; an in-memory value is not the sealed byte receipt.
        let manifest = self.outbox.verify(&identity.observation_id).await?;
        let mut records = Vec::new();
        let source = manifest.description.as_ref().ok_or_else(|| invalid("native source descriptor absent"))?;
        for file in &manifest.files {
            let bytes = bounded_read(&output.join(&file.file), file.bytes as usize)?;
            if file.sha256.as_deref() != Some(digest(&bytes).as_str()) {
                return Err(invalid("native sealed stream digest differs from its durable manifest"));
            }
            for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
                let row: ExportedRecord =
                    serde_json::from_slice(line).map_err(|_| invalid("native export row could not be decoded"))?;
                if row.record.stream() != file.stream
                    || row != ExportedRecord::new(source, &identity, row.record.clone())?
                {
                    return Err(invalid("native export row differs from its actual source/capture identity"));
                }
                records.push(row);
                if records.len() > manifest.request.budget.max_snapshot_rows as usize {
                    return Err(invalid("native record count exceeded the original source budget"));
                }
            }
        }
        let manifest_path = output.join("manifest.json");
        let manifest_bytes = bounded_read(&manifest_path, MAX_MANIFEST_BYTES)?;
        Ok(Captured {
            manifest,
            records,
            manifest_sha256: digest(&manifest_bytes),
            manifest_path,
            capture_export_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub async fn verify_retained(&mut self, capture: &Captured) -> Result<()> {
        self.verify_manifest(&capture.manifest).await?;
        if digest(&bounded_read(&capture.manifest_path, MAX_MANIFEST_BYTES)?) != capture.manifest_sha256 {
            return Err(invalid("original durable manifest bytes changed during holder followup"));
        }
        Ok(())
    }

    async fn verify_manifest(&mut self, expected: &CollectionManifest) -> Result<()> {
        let actual = self.outbox.verify(&expected.capture.observation_id).await?;
        let encoded = |manifest: &CollectionManifest| {
            serde_json::to_value(manifest).map_err(|_| invalid("native manifest could not be encoded"))
        };
        if encoded(&actual)? != encoded(expected)? {
            return Err(invalid("original durable parent changed during holder followup"));
        }
        Ok(())
    }
}

impl Captured {
    pub fn source(&self) -> Result<&CollectionDescription> {
        self.manifest.description.as_ref().ok_or_else(|| invalid("captured native source absent"))
    }
    pub fn capture_ref(&self) -> Result<DiagnosticCaptureRef> {
        Ok(self.reference(0)?.capture_ref())
    }

    pub fn activity_context(&self) -> Result<NativeReadContext> {
        NativeReadContext::from_activity(self.source()?)
    }

    pub fn with_activity_frame<T>(&self, inspect: impl FnOnce(&ActivityFrameEvidence<'_>) -> Result<T>) -> Result<T> {
        let references = (0..self.records.len()).map(|index| self.reference(index)).collect::<Result<Vec<_>>>()?;
        let rows = self
            .records
            .iter()
            .zip(&references)
            .map(|(row, reference)| {
                let TelemetryRecord::Activity(observation) = &row.record else {
                    return Err(invalid("original native Activity row absent"));
                };
                Ok(ActivityFrameRow { observation, reference: Some(reference) })
            })
            .collect::<Result<Vec<_>>>()?;
        inspect(&ActivityFrameEvidence::new(self.source()?, &rows, true)?)
    }

    pub fn reference(&self, index: usize) -> Result<DiagnosticEvidenceRef> {
        let row = self.records.get(index).ok_or_else(|| invalid("native selected record absent"))?;
        let ordinal = self.records[..index].iter().filter(|prior| prior.record.stream() == row.record.stream()).count();
        DiagnosticEvidenceRef::from_export(self.source()?, &self.manifest_sha256, row, ordinal as u32)
    }
}

fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| invalid("owned capture artifact could not be opened"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("owned capture artifact could not be read"))?;
    if bytes.len() > limit {
        return Err(invalid("owned capture artifact exceeded its original byte bound"));
    }
    Ok(bytes)
}
