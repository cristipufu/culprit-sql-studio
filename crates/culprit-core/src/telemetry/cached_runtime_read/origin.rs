//! Original captured positions are never rewritten as native read positions or repaired after masking.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum CachedRuntimeReadOrigin {
    /// Opaque identity issued by the native read owner; not an epoch or an authenticated archive receipt.
    NativeRead { read_id: String },
    /// The source digest pins the original descriptor, including its independent read clocks and coverage.
    /// Compact row positions pin canonical exported bytes without repeating capture/source/native-key tuples.
    Captured { capture: DiagnosticCaptureRef, source_sha256: String, positions: Vec<CapturedSnapshotPosition> },
}

impl CachedRuntimeReadOrigin {
    /// The caller must authenticate the original manifest and exported positions before supplying them.
    pub fn captured(
        source: &CollectionDescription,
        capture: DiagnosticCaptureRef,
        positions: Vec<CapturedSnapshotPosition>,
    ) -> Result<Self> {
        Ok(Self::Captured {
            capture,
            source_sha256: super::super::captured_snapshot::source_digest(source)?,
            positions,
        })
    }

    pub(super) fn read_id(&self) -> Result<String> {
        match self {
            Self::NativeRead { read_id } if valid_digest(read_id) => Ok(read_id.clone()),
            Self::Captured { capture, .. } if capture.is_valid() => cached_statement_read_id(&capture.capture),
            _ => Err(invalid()),
        }
    }

    pub(super) fn matches(
        &self,
        source: &CollectionDescription,
        rows: &[OperationalObservation],
        read_complete: bool,
    ) -> bool {
        match self {
            Self::NativeRead { read_id } => valid_digest(read_id),
            Self::Captured { capture, source_sha256, positions } => {
                runtime_snapshot(source).is_some()
                    && super::super::captured_snapshot::visit_rows(
                        source,
                        capture,
                        source_sha256,
                        positions,
                        rows.iter().cloned().map(TelemetryRecord::Operational),
                        read_complete,
                        |_| {},
                    )
                    .is_ok()
            }
        }
    }

    pub(super) fn references(
        &self,
        source: &CollectionDescription,
        rows: &[OperationalObservation],
        read_complete: bool,
    ) -> Result<Option<Vec<DiagnosticEvidenceRef>>> {
        match self {
            Self::NativeRead { read_id } if valid_digest(read_id) => Ok(None),
            Self::Captured { capture, source_sha256, positions } => {
                runtime_snapshot(source).ok_or_else(invalid)?;
                super::super::captured_snapshot::references(
                    source,
                    capture,
                    source_sha256,
                    positions,
                    rows.iter().cloned().map(TelemetryRecord::Operational),
                    read_complete,
                )
                .map(Some)
            }
            _ => Err(invalid()),
        }
    }
}
