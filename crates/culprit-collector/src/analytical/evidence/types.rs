pub use culprit_core::collected_analysis::{EvidenceGap, EvidenceGapReason};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceFamily {
    ResourceSamples,
    OperationalSamples,
    QueryDefinitions,
    PlanDefinitions,
    QueryRuntime,
    QueryWaits,
}
impl EvidenceFamily {
    pub const ALL: &[Self] = &[
        Self::ResourceSamples,
        Self::OperationalSamples,
        Self::QueryDefinitions,
        Self::PlanDefinitions,
        Self::QueryRuntime,
        Self::QueryWaits,
    ];

    pub fn stream(self) -> TelemetryStream {
        match self {
            Self::ResourceSamples => TelemetryStream::ResourceSamples,
            Self::OperationalSamples => TelemetryStream::OperationalSamples,
            Self::QueryDefinitions => TelemetryStream::QueryDefinitions,
            Self::PlanDefinitions => TelemetryStream::PlanDefinitions,
            Self::QueryRuntime => TelemetryStream::QueryRuntime,
            Self::QueryWaits => TelemetryStream::QueryWaits,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EvidenceReadBudget {
    pub page_rows: u32,
    /// Maximum native query response body and decoded row encoding, including publication and audit reads.
    pub page_bytes: u32,
    pub max_rows: u32,
    pub max_queries: u32,
    /// Cumulative complete native query response bodies, including metadata and completion frames.
    pub max_bytes: u64,
    pub max_proof_bytes: u64,
    /// Maximum caller wait for the whole read; native transports retain their own server/HTTP ceilings.
    pub timeout_ms: u32,
}
impl Default for EvidenceReadBudget {
    fn default() -> Self {
        Self {
            page_rows: 250,
            page_bytes: 4 * 1024 * 1024,
            max_rows: 10_000,
            max_queries: 128,
            max_bytes: 32 * 1024 * 1024,
            max_proof_bytes: 4 * 1024 * 1024,
            timeout_ms: 60_000,
        }
    }
}
impl EvidenceReadBudget {
    pub fn validate(&self) -> Result<()> {
        if !(1..=10_000).contains(&self.page_rows)
            || !(1024..=12 * 1024 * 1024).contains(&self.page_bytes)
            || !(1..=100_000).contains(&self.max_rows)
            || !(1..=1024).contains(&self.max_queries)
            || self.max_bytes < u64::from(self.page_bytes)
            || self.max_bytes > 256 * 1024 * 1024
            || self.max_proof_bytes == 0
            || self.max_proof_bytes > 16 * 1024 * 1024
            || !(1..=300_000).contains(&self.timeout_ms)
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EvidenceReadMetrics {
    pub queries: u32,
    pub pages: u32,
    pub returned_rows: u32,
    pub response_bytes: u64,
    pub proof_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifiedEvidenceRecord {
    pub(super) batch_id: String,
    pub(super) row_ordinal: u32,
    pub(super) native_payload_sha256: String,
    pub(super) row: ExportedRecord,
}
impl VerifiedEvidenceRecord {
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }
    pub fn row_ordinal(&self) -> u32 {
        self.row_ordinal
    }
    pub fn native_payload_sha256(&self) -> &str {
        &self.native_payload_sha256
    }
    pub fn row(&self) -> &ExportedRecord {
        &self.row
    }
}

/// A read of the requested families of this exact capture. Native source coverage, freshness, continuity and
/// counter epochs remain separate facts in the source and records.
#[derive(Clone, Debug, Serialize)]
pub struct EvidenceCapture {
    pub(super) range_id: String,
    pub(super) manifest_sha256: String,
    pub(super) source: CollectionDescription,
    pub(super) capture: CaptureIdentity,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) families: Vec<EvidenceFamily>,
    pub(super) records: Vec<VerifiedEvidenceRecord>,
    pub(super) gaps: Vec<EvidenceGap>,
    pub(super) metrics: EvidenceReadMetrics,
}
impl EvidenceCapture {
    pub fn range_id(&self) -> &str {
        &self.range_id
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.source
    }
    pub fn capture(&self) -> &CaptureIdentity {
        &self.capture
    }
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    pub fn families(&self) -> &[EvidenceFamily] {
        &self.families
    }
    pub fn records(&self) -> &[VerifiedEvidenceRecord] {
        &self.records
    }
    pub fn gaps(&self) -> &[EvidenceGap] {
        &self.gaps
    }
    pub fn metrics(&self) -> &EvidenceReadMetrics {
        &self.metrics
    }
    /// Every requested stored row was reconstructed without readback gaps. Native sources may still have
    /// partial scope or be unavailable; inspect `source().observation` independently.
    pub fn is_read_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}
