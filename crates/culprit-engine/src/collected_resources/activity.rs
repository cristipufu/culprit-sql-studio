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
            || capture.source().observation.activity().is_none()
        {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        super::references::validate_capture(capture)?;
        let mut positions = BTreeSet::new();
        let references = capture
            .records()
            .iter()
            .map(|record| {
                let reference = super::references::from_record(capture, record)?;
                if !matches!(record.row().record, TelemetryRecord::Activity(_))
                    || !positions.insert((record.batch_id(), reference.row_ordinal))
                {
                    return Err(invalid());
                }
                Ok(reference)
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
