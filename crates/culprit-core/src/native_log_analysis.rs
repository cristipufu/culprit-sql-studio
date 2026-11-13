//! Local supplied-log analysis is neither a verified external feed nor a database snapshot.
use crate::{RetainedFailureReport, SourceOutcome, SourceState, telemetry::NativeFailureSnapshot};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeLogFormat {
    PostgresJson,
    PostgresCsv13,
    PostgresCsv,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeLogLimits {
    pub input_bytes: usize,
    pub record_bytes: usize,
    pub field_bytes: usize,
    pub records: usize,
}
impl Default for NativeLogLimits {
    fn default() -> Self {
        Self { input_bytes: 1024 * 1024, record_bytes: 64 * 1024, field_bytes: 32 * 1024, records: 1024 }
    }
}
impl NativeLogLimits {
    pub fn is_valid(self) -> bool {
        (1..=8 * 1024 * 1024).contains(&self.input_bytes)
            && (1..=256 * 1024).contains(&self.record_bytes)
            && (1..=64 * 1024).contains(&self.field_bytes)
            && (1..=4096).contains(&self.records)
            && self.field_bytes <= self.record_bytes
            && self.record_bytes <= self.input_bytes
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "field", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeLogProblem {
    InvalidLimits,
    InputLimit,
    InputChanged,
    RecordLimit,
    RecordBytes,
    FieldBytes,
    InvalidUtf8,
    MalformedRecord,
    DuplicateField,
    InvalidField(String),
    CsvColumnCount,
    NativeShape,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeLogIssue {
    /// Zero-based supplied record position, not an occurrence identity across acquisitions.
    pub position: u32,
    pub problem: NativeLogProblem,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeLogArtifact {
    pub format: NativeLogFormat,
    /// SHA-256 of exactly the supplied/read prefix. This identifies an artifact, never a native incident or host.
    pub sha256: String,
    pub bytes: u64,
    /// The whole supplied buffer/file was read without a known change. This does not promise an atomic native log.
    pub complete: bool,
    pub limitation: Option<NativeLogProblem>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeLogAnalysis {
    pub input: NativeLogArtifact,
    /// UTC capture bounds describe local intake, independently from each native event clock. Resource is unowned.
    pub evidence: NativeFailureSnapshot,
    pub source: SourceOutcome,
    pub records_examined: u32,
    pub non_failure_records: u32,
    pub issues: Vec<NativeLogIssue>,
    pub stopped: Option<NativeLogProblem>,
    pub report: RetainedFailureReport,
}
impl NativeLogAnalysis {
    /// Only the requested local intake/decoding is complete. Native retention, incident counts and ownership remain
    /// unqualified even when this is true or the supplied artifact has no failure rows.
    pub fn is_read_complete(&self) -> bool {
        self.input.complete
            && self.issues.is_empty()
            && self.stopped.is_none()
            && matches!(self.source.state, SourceState::Collected | SourceState::Empty)
    }
}
