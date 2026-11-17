//! Client-observed work of one capture, separate from native database cost and archive readback work.
use crate::{
    Engine,
    collected_analysis::{CollectedEvidenceMetrics, EvidenceGap},
    telemetry::{CaptureIdentity, CollectionManifest, CollectionStatus, ReadWorkMetrics},
};
use serde::Serialize;
use ts_rs::TS;

/// The caller authenticates this metadata. Collector acquisition clocks never describe the native history window.
/// Missing query totals and metrics remain unknown; tracked template tasks are not a replacement for either.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct SourceOverheadEvidence {
    pub capture: CaptureIdentity,
    pub engine: Option<Engine>,
    pub status: CollectionStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub source_queries: Option<u32>,
    pub read_metrics: Option<ReadWorkMetrics>,
}

impl SourceOverheadEvidence {
    /// An unfinished manifest's initial query count is a placeholder, not a measured zero.
    pub fn from_manifest(manifest: &CollectionManifest) -> Self {
        Self {
            capture: manifest.capture.clone(),
            engine: manifest.description.as_ref().map(|source| source.target.engine()),
            status: manifest.status.clone(),
            started_at: manifest.started_at.clone(),
            finished_at: manifest.finished_at.clone(),
            source_queries: manifest.finished_at.as_ref().map(|_| manifest.source_queries),
            read_metrics: manifest.read_metrics.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct SourceOverheadReport {
    /// Numerical measurements occur once. SQL previews are withheld; authentication does not establish safe text.
    pub evidence: SourceOverheadEvidence,
    /// Indices into evidence.read_metrics.templates, ordered by descending client elapsed time, then template SHA.
    pub template_order: Vec<u32>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
}

/// This metadata-only archive read has no selected native rows or payload recovery. Its metrics describe archive I/O.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedSourceOverheadAnalysis {
    pub range_id: String,
    pub recovery_manifest_sha256: String,
    pub capture_manifest_sha256: String,
    pub read_gaps: Vec<EvidenceGap>,
    pub metrics: CollectedEvidenceMetrics,
    pub report: SourceOverheadReport,
}
