//! Captured positions preserve original bytes without granting unresolved rows a semantic identity.
use super::{
    diagnostics::{digest_value, valid_capture},
    *,
};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedStreamRows {
    pub stream: TelemetryStream,
    /// Original selected manifest population, independent of how many rows were reconstructed.
    pub rows: u32,
}

/// One original capture/source binding shared by compact positions. Structural validity does not authenticate it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedReadRef {
    pub capture: CaptureIdentity,
    pub manifest_sha256: String,
    pub source_sha256: String,
    pub streams: Vec<CapturedStreamRows>,
}
impl CapturedReadRef {
    /// The owner supplies original manifest counts, never counts inferred from a partial body.
    pub fn new(
        source: &CollectionDescription,
        capture: CaptureIdentity,
        manifest_sha256: String,
        streams: Vec<CapturedStreamRows>,
    ) -> Result<Self> {
        let value = Self { capture, manifest_sha256, source_sha256: digest_value(source)?, streams };
        value.validate_source(source)?;
        Ok(value)
    }

    pub fn is_valid(&self) -> bool {
        valid_capture(&self.capture)
            && valid_digest(&self.manifest_sha256)
            && valid_digest(&self.source_sha256)
            && !self.streams.is_empty()
            && self.streams.len() <= TelemetryStream::ALL.len()
            && self
                .streams
                .iter()
                .enumerate()
                .all(|(index, stream)| !self.streams[..index].iter().any(|earlier| earlier.stream == stream.stream))
    }

    pub fn captured_rows(&self, stream: TelemetryStream) -> Option<u32> {
        self.streams.iter().find(|selected| selected.stream == stream).map(|selected| selected.rows)
    }

    pub fn validate_source(&self, source: &CollectionDescription) -> Result<()> {
        if !self.is_valid()
            || !source.partition_is_consistent()
            || source.captured_range().is_err()
            || digest_value(source)? != self.source_sha256
            || self.streams.iter().any(|selected| !source.observation.collection().streams().contains(&selected.stream))
            || self
                .streams
                .iter()
                .any(|selected| declared_rows(source, selected.stream).is_some_and(|rows| rows != selected.rows))
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Validate the shared source once before processing bounded original rows. This is structural binding,
    /// not a storage authenticator or permission to collect data.
    pub fn bind<'a>(&'a self, source: &'a CollectionDescription) -> Result<CapturedReadBinding<'a>> {
        self.validate_source(source)?;
        Ok(CapturedReadBinding { parent: self, source })
    }

    fn validate_position(&self, stream: TelemetryStream, ordinal: u32) -> Result<()> {
        if !self.is_valid() || self.captured_rows(stream).is_none_or(|rows| ordinal >= rows) {
            return Err(invalid());
        }
        Ok(())
    }
}

/// A zero-based original stream position. Its batch is derived from the parent capture and this stream.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedPosition {
    pub stream: TelemetryStream,
    pub row_ordinal: u32,
    pub payload_sha256: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum CapturedRecordReference {
    Diagnostic {
        reference: Box<DiagnosticEvidenceRef>,
    },
    /// Informational only: the original native row has no resolved semantic record ID.
    Informational {
        position: CapturedPosition,
    },
}
impl CapturedRecordReference {
    /// The caller authenticates the capture and original membership independently. A resolved record never
    /// falls back to an informational position when its diagnostic reference is invalid.
    pub fn from_export(
        parent: &CapturedReadRef,
        source: &CollectionDescription,
        row: &ExportedRecord,
        row_ordinal: u32,
    ) -> Result<Self> {
        parent.bind(source)?.reference(row, row_ordinal)
    }

    /// Match unchanged original bytes, not a repaired canonical body. This grants no storage authentication,
    /// query totals, native continuity, dispatch permission or incident ownership.
    pub fn validate_canonical_record(
        &self,
        parent: &CapturedReadRef,
        source: &CollectionDescription,
        record: &TelemetryRecord,
    ) -> Result<()> {
        parent.bind(source)?.validate_canonical_record(self, record)
    }

    pub fn stream(&self) -> TelemetryStream {
        match self {
            Self::Diagnostic { reference } => reference.stream,
            Self::Informational { position } => position.stream,
        }
    }

    pub fn row_ordinal(&self) -> u32 {
        match self {
            Self::Diagnostic { reference } => reference.row_ordinal,
            Self::Informational { position } => position.row_ordinal,
        }
    }

    pub fn is_informational(&self) -> bool {
        matches!(self, Self::Informational { .. })
    }
}

/// One checked shared descriptor for a row loop. It borrows original input and owns no authentication state.
pub struct CapturedReadBinding<'a> {
    parent: &'a CapturedReadRef,
    source: &'a CollectionDescription,
}
impl CapturedReadBinding<'_> {
    pub fn reference(&self, row: &ExportedRecord, row_ordinal: u32) -> Result<CapturedRecordReference> {
        let (parent, source) = (self.parent, self.source);
        parent.validate_position(row.record.stream(), row_ordinal)?;
        if row.capture != parent.capture {
            return Err(invalid());
        }
        if row.record_id.is_some() {
            let reference = DiagnosticEvidenceRef::from_export(source, &parent.manifest_sha256, row, row_ordinal)?;
            reference.validate_canonical_record(source, &row.record)?;
            return Ok(CapturedRecordReference::Diagnostic { reference: Box::new(reference) });
        }
        if !unresolved_statement(&row.record) {
            return Err(invalid());
        }
        let canonical = ExportedRecord::new(source, &parent.capture, row.record.clone())?;
        if row.payload_sha256()? != canonical.payload_sha256()? {
            return Err(invalid());
        }
        Ok(CapturedRecordReference::Informational {
            position: CapturedPosition {
                stream: row.record.stream(),
                row_ordinal,
                payload_sha256: row.payload_sha256()?,
            },
        })
    }

    pub fn validate_canonical_record(
        &self,
        reference: &CapturedRecordReference,
        record: &TelemetryRecord,
    ) -> Result<()> {
        let (parent, source) = (self.parent, self.source);
        parent.validate_position(reference.stream(), reference.row_ordinal())?;
        match reference {
            CapturedRecordReference::Diagnostic { reference } => {
                if reference.capture != parent.capture || reference.manifest_sha256 != parent.manifest_sha256 {
                    return Err(invalid());
                }
                reference.validate_canonical_record(source, record)
            }
            CapturedRecordReference::Informational { position } => {
                let canonical = ExportedRecord::new(source, &parent.capture, record.clone())?;
                if digest_value(record)? != digest_value(&canonical.record)?
                    || self.reference(&canonical, position.row_ordinal)? != *reference
                {
                    return Err(invalid());
                }
                Ok(())
            }
        }
    }
}

fn unresolved_statement(record: &TelemetryRecord) -> bool {
    match record {
        TelemetryRecord::QueryRuntime(row) => !row.key.references.is_resolved(),
        TelemetryRecord::QueryWaits(row) => !row.key.references.is_resolved(),
        _ => false,
    }
}

fn declared_rows(source: &CollectionDescription, stream: TelemetryStream) -> Option<u32> {
    source
        .observation
        .activity()
        .filter(|_| stream == TelemetryStream::ActivityObservations)
        .map(|activity| activity.observations)
        .or_else(|| {
            source
                .observation
                .snapshot()
                .filter(|snapshot| snapshot.dataset.stream() == stream)
                .map(|snapshot| snapshot.records)
        })
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
