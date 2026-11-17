//! The immutable native result owns its original participant and roster even when warehouse rows are missing.
use super::*;
use culprit_core::{MaintenanceProgressReport, collected_resource_analysis::CollectedMaintenanceProgressAnalysis};

pub fn maintenance_progress(capture: &EvidenceCapture) -> Result<CollectedMaintenanceProgressAnalysis> {
    let evidence = CapturedMaintenance::new(capture)?;
    let mut report = culprit_checks::maintenance_progress::explain(&evidence.snapshot, capture.is_read_complete())
        .map_err(|_| invalid())?;
    evidence.qualify_report(&mut report);
    Ok(CollectedMaintenanceProgressAnalysis { context: crate::collected_context::from_capture(capture), report })
}

pub(super) struct CapturedMaintenance<'a> {
    capture: &'a EvidenceCapture,
    pub snapshot: MaintenanceProgressSnapshot,
    outcome: SourceOutcome,
}

impl<'a> CapturedMaintenance<'a> {
    pub(super) fn new(capture: &'a EvidenceCapture) -> Result<Self> {
        let SourceObservation::MaintenanceProgress { snapshot: native, result } = &capture.source().observation else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        if !result.is_valid_source(capture.source(), native) || !capture.source().partition_is_consistent() {
            return Err(invalid());
        }
        super::references::validate_capture(capture)?;
        let input = CapturedState::new(capture, SnapshotDataset::MaintenanceProgress)?;
        input.database()?;
        let observations = input
            .rows()?
            .into_iter()
            .map(|row| {
                let OperationalFact::MaintenanceProgress(point) = &row.fact else { return Err(invalid()) };
                if point.key()? != row.key || !point.fits_source_scope(capture.source()) {
                    return Err(invalid());
                }
                Ok(point.as_ref().clone())
            })
            .collect::<Result<Vec<_>>>()?;
        let outcome = input.outcome(observations.len())?;
        let snapshot = MaintenanceProgressSnapshot {
            source: input.coverage.source.clone(),
            context: result.read_context(capture.source(), native),
            coverage: input.coverage.clone(),
            result: result.as_ref().clone(),
            observations,
        };
        if !snapshot.is_valid_read(capture.is_read_complete()) {
            return Err(invalid());
        }
        Ok(Self { capture, snapshot, outcome })
    }

    /// Receipt linkage is required only for composition; standalone native points do not need durable revisions.
    pub(super) fn observation_reference(&self) -> Result<Option<DiagnosticEvidenceRef>> {
        match self.capture.records() {
            [] => Ok(None),
            [record] => super::references::from_record(self.capture, record).map(Some),
            _ => Err(invalid()),
        }
    }

    pub(super) fn qualify_report(&self, report: &mut MaintenanceProgressReport) {
        report.source_state = self.outcome.state;
        report.source_reason = self.outcome.reason.clone();
    }
}
