//! Source-reader measurements and their bounded interpretations, separate from archive readback work.
use crate::{
    Engine,
    collected_analysis::{CollectedEvidenceMetrics, EvidenceGap},
    telemetry::{
        CaptureIdentity, CollectionDescription, CollectionManifest, CollectionStatus, ReadWorkMetrics, ReadWorkSummary,
    },
};
use serde::Serialize;
use ts_rs::TS;

/// The caller authenticates this metadata. Collector acquisition clocks never describe the native history window.
/// Missing query totals and metrics remain unknown; tracked template tasks are not a replacement for either.
#[derive(Clone, Debug)]
pub struct SourceOverheadEvidence {
    pub capture: CaptureIdentity,
    pub engine: Option<Engine>,
    pub source: Option<CollectionDescription>,
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
            source: manifest.description.clone(),
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
pub struct SourceOverheadCaptureSummary {
    pub capture: CaptureIdentity,
    pub engine: Option<Engine>,
    pub status: CollectionStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub source_queries: Option<u32>,
    pub read_metrics: Option<ReadWorkSummary>,
}
impl SourceOverheadEvidence {
    pub fn summary(&self) -> crate::Result<SourceOverheadCaptureSummary> {
        Ok(SourceOverheadCaptureSummary {
            capture: self.capture.clone(),
            engine: self.engine,
            status: self.status.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
            source_queries: self.source_queries,
            read_metrics: self
                .read_metrics
                .as_ref()
                .map(|metrics| metrics.summary(self.source.as_ref()))
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct SourceNativeIntervalExplanation {
    pub summary: String,
    pub measurements: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct SourceOverheadReport {
    /// Numerical measurements occur once. SQL previews are withheld; authentication does not establish safe text.
    pub evidence: SourceOverheadCaptureSummary,
    /// Indices into evidence.read_metrics.templates, ordered by descending client elapsed time, then template SHA.
    pub template_order: Vec<u32>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub native_interval: Option<SourceNativeIntervalExplanation>,
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
