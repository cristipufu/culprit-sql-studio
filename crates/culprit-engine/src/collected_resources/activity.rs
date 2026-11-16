//! Original Activity receipts are derived once for maintenance and page participation, without another read.
use super::*;
use std::collections::BTreeSet;

pub(super) struct CapturedActivity<'a> {
    capture: &'a EvidenceCapture,
    references: Vec<DiagnosticEvidenceRef>,
}

impl<'a> CapturedActivity<'a> {
    pub(super) fn new(capture: &'a EvidenceCapture) -> Result<Self> {
        if !capture.families().contains(&EvidenceFamily::ActivityObservations)
            || !matches!(capture.source().observation, SourceObservation::Activity(_))
        {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        if !valid_digest(capture.range_id())
            || !valid_digest(capture.recovery_manifest_sha256())
            || !valid_digest(capture.capture_manifest_sha256())
            || ObservationBounds::new(capture.collected_from(), capture.collected_until()).is_none()
        {
            return Err(invalid());
        }
        let mut positions = BTreeSet::new();
        let references = capture
            .records()
            .iter()
            .map(|record| {
                let row = record.row();
                let ordinal = record.row_ordinal().checked_sub(1).ok_or_else(invalid)?;
                if !matches!(row.record, TelemetryRecord::Activity(_))
                    || row.capture != *capture.capture()
                    || row.batch_id != record.batch_id()
                    || !positions.insert((record.batch_id(), ordinal))
                    || row.payload_sha256()? != record.native_payload_sha256()
                {
                    return Err(invalid());
                }
                DiagnosticEvidenceRef::from_export(capture.source(), capture.capture_manifest_sha256(), row, ordinal)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { capture, references })
    }

    pub(super) fn rows(&self) -> Result<Vec<ActivityFrameRow<'_>>> {
        self.capture
            .records()
            .iter()
            .zip(&self.references)
            .map(|(record, reference)| {
                let TelemetryRecord::Activity(observation) = &record.row().record else { return Err(invalid()) };
                Ok(ActivityFrameRow { observation, reference: Some(reference) })
            })
            .collect()
    }

    pub(super) fn frame(&self) -> Result<ActivityFrameEvidence<'_>> {
        ActivityFrameEvidence::new(self.capture.source(), &self.rows()?, self.capture.is_read_complete())
    }
}
