//! Source-reader measurements and their bounded interpretations, separate from archive readback work.
use crate::{
    Engine,
    collected_analysis::{CollectedEvidenceMetrics, EvidenceGap},
    telemetry::{
        CaptureIdentity, CollectionDescription, CollectionManifest, CollectionStatus, ReadWorkMetrics, ReadWorkSummary,
    },
};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct SourceOverheadCaptureSummary {
    pub capture: CaptureIdentity,
    #[serde(deserialize_with = "required_summary_field")]
    pub engine: Option<Engine>,
    pub status: CollectionStatus,
    pub started_at: String,
    #[serde(deserialize_with = "required_summary_field")]
    pub finished_at: Option<String>,
    #[serde(deserialize_with = "required_summary_field")]
    pub source_queries: Option<u32>,
    #[serde(deserialize_with = "required_summary_field")]
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

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceNativeIntervalExplanation {
    pub summary: String,
    pub measurements: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceConnectionExplanation {
    pub summary: String,
    pub observations: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceOverheadReport {
    pub connection_work: Option<SourceConnectionExplanation>,
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

impl SourceOverheadCaptureSummary {
    pub fn is_valid(&self) -> bool {
        let until = self.finished_at.as_deref().unwrap_or(&self.started_at);
        let Some(bounds) = crate::telemetry::ObservationBounds::new(&self.started_at, until) else { return false };
        self.started_at.len() <= 64
            && until.len() <= 64
            && (self.status != CollectionStatus::Complete || self.finished_at.is_some())
            && (self.source_queries.is_none() || self.finished_at.is_some())
            && self.read_metrics.as_ref().is_none_or(|metrics| {
                metrics.is_valid()
                    && metrics.connection_work.as_ref().is_none_or(|work| {
                        work.attempts.iter().flat_map(|attempt| &attempt.phases).all(|phase| {
                            bounds.contains(&phase.client_started_at)
                                && phase.client_finished_at.as_ref().is_none_or(|at| bounds.contains(at))
                        })
                    })
            })
    }
}

fn required_summary_field<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}
