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
    Captured { capture: DiagnosticCaptureRef, source_sha256: String, positions: Vec<CachedRuntimeCapturedPosition> },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CachedRuntimeCapturedPosition {
    pub row_ordinal: u32,
    pub payload_sha256: String,
}
impl CachedRuntimeCapturedPosition {
    pub fn from_export(row: &ExportedRecord, row_ordinal: u32) -> Result<Self> {
        Ok(Self { row_ordinal, payload_sha256: row.payload_sha256()? })
    }
}

impl CachedRuntimeReadOrigin {
    /// The caller must authenticate the original manifest and exported positions before supplying them.
    pub fn captured(
        source: &CollectionDescription,
        capture: DiagnosticCaptureRef,
        positions: Vec<CachedRuntimeCapturedPosition>,
    ) -> Result<Self> {
        Ok(Self::Captured { capture, source_sha256: source_digest(source)?, positions })
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
                valid_header(source, capture, source_sha256, positions, rows.len(), read_complete).is_ok()
                    && rows
                        .iter()
                        .zip(positions)
                        .all(|(row, position)| reference(source, capture, row, position).is_ok())
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
                valid_header(source, capture, source_sha256, positions, rows.len(), read_complete)?;
                rows.iter()
                    .zip(positions)
                    .map(|(row, position)| reference(source, capture, row, position))
                    .collect::<Result<Vec<_>>>()
                    .map(Some)
            }
            _ => Err(invalid()),
        }
    }
}

fn valid_header(
    source: &CollectionDescription,
    capture: &DiagnosticCaptureRef,
    source_sha256: &str,
    positions: &[CachedRuntimeCapturedPosition],
    rows: usize,
    read_complete: bool,
) -> Result<()> {
    let snapshot = runtime_snapshot(source).ok_or_else(invalid)?;
    let mut ordinals = BTreeSet::new();
    if !capture.is_valid()
        || !valid_digest(source_sha256)
        || source_digest(source)? != source_sha256
        || source.captured_range()?.scope_id != capture.source_scope_id
        || positions.len() != rows
        || positions.len() > CollectionBudget::upper_limits().max_snapshot_rows as usize
        || positions.iter().any(|position| {
            !valid_digest(&position.payload_sha256)
                || position.row_ordinal >= snapshot.records
                || !ordinals.insert(position.row_ordinal)
        })
        || read_complete && positions.len() != snapshot.records as usize
    {
        return Err(invalid());
    }
    Ok(())
}

fn reference(
    source: &CollectionDescription,
    capture: &DiagnosticCaptureRef,
    row: &OperationalObservation,
    position: &CachedRuntimeCapturedPosition,
) -> Result<DiagnosticEvidenceRef> {
    let record = TelemetryRecord::Operational(row.clone());
    let exported = ExportedRecord::new(source, &capture.capture, record.clone())?;
    if exported.payload_sha256()? != position.payload_sha256 {
        return Err(invalid());
    }
    let reference =
        DiagnosticEvidenceRef::from_export(source, &capture.manifest_sha256, &exported, position.row_ordinal)?;
    reference.validate_canonical_record(source, &record)?;
    Ok(reference)
}

fn source_digest(source: &CollectionDescription) -> Result<String> {
    serde_json::to_vec(source).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}
