//! The selected original body comes from the same canonical verification pass, before any receipt is returned.
use super::*;
use crate::outbox::artifacts::{CanonicalRows, MAX_CANONICAL_ROWS, VerificationObserver};

pub(super) enum RowSelection {
    All,
    Diagnostic,
    SharedOwner,
}

struct DiagnosticRows {
    rows: CanonicalRows,
    selection: RowSelection,
    selected: bool,
}
impl VerificationObserver for DiagnosticRows {
    fn manifest(&mut self, manifest: &CollectionManifest) -> Result<()> {
        let source = manifest.description.as_ref().ok_or_else(integrity)?;
        self.selected = match self.selection {
            RowSelection::All => true,
            RowSelection::Diagnostic => !matches!(source.observation, SourceObservation::SourceRead { .. }),
            RowSelection::SharedOwner => shared_owner_source(source),
        };
        Ok(())
    }
    fn row(&mut self, row: &ExportedRecord, ordinal: u32) -> Result<()> {
        if self.selected && !self.rows.retain(row, ordinal) {
            return Err(integrity());
        }
        Ok(())
    }
}

impl Files {
    pub(super) fn validated_rows(
        &mut self,
        id: &str,
        selection: RowSelection,
    ) -> Result<(ValidatedCapture, CanonicalRows)> {
        let mut read = DiagnosticRows { rows: CanonicalRows::new(MAX_CANONICAL_ROWS), selection, selected: false };
        let validated = self.validated_observed(id, &mut read)?;
        Ok((validated, read.rows))
    }
}

pub(super) fn referenced_rows(
    parent: &DiagnosticCaptureRef,
    source: &CollectionDescription,
    rows: CanonicalRows,
) -> Result<Vec<DiagnosticInputRow>> {
    let (records, ordinals) = rows.into_parts();
    let rows = row_references(parent, source, records)?;
    if rows.iter().zip(ordinals).any(|(row, ordinal)| row.reference.row_ordinal != ordinal) {
        return Err(integrity());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn originals() -> (DiagnosticCaptureRef, CollectionDescription, Vec<ExportedRecord>) {
        let (source, records) = crate::analytical::test_support::activity_evidence_cases()
            .into_iter()
            .find(|(_, rows)| rows.len() > 1)
            .unwrap();
        let capture = CaptureIdentity {
            observation_id: "a".repeat(32),
            revision: Some(CaptureRevision {
                outbox_id: "b".repeat(32),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let parent = DiagnosticCaptureRef {
            capture: capture.clone(),
            manifest_sha256: digest(b"structural-row-position-test"),
            source_scope_id: source.captured_range().unwrap().scope_id,
        };
        let rows = records.into_iter().map(|record| ExportedRecord::new(&source, &capture, record).unwrap()).collect();
        (parent, source, rows)
    }

    #[test]
    fn an_original_ordinal_is_never_replaced_by_a_filtered_buffers_index() {
        let (parent, source, rows) = originals();
        let mut all = CanonicalRows::new(MAX_CANONICAL_ROWS);
        for (ordinal, row) in rows.iter().enumerate() {
            assert!(all.retain(row, ordinal as u32));
        }
        let references = referenced_rows(&parent, &source, all).unwrap();
        assert_eq!(references[0].reference.row_ordinal, 0);
        assert_eq!(references[1].reference.row_ordinal, 1);
        let mut filtered = CanonicalRows::new(MAX_CANONICAL_ROWS);
        assert!(filtered.retain(&rows[1], 1));
        assert!(
            referenced_rows(&parent, &source, filtered).is_err(),
            "a later row cannot be resigned as the first row merely because it is the only retained body"
        );
    }

    #[test]
    fn a_full_buffer_refuses_more_rows_without_replacing_the_original_or_its_position() {
        let (_, _, rows) = originals();
        let mut bounded = CanonicalRows::new(1);
        assert!(bounded.retain(&rows[0], 0));
        assert!(!bounded.retain(&rows[1], 1));
        let (retained, ordinals) = bounded.into_parts();
        assert_eq!(retained.len(), 1);
        assert_eq!(ordinals, [0]);
        assert_eq!(retained[0].payload_sha256().unwrap(), rows[0].payload_sha256().unwrap());
    }
}
