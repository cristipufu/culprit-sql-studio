//! Archive receipts qualify the retained Activity frame; they do not establish which request acquired a lock.
use super::*;
use culprit_checks::maintenance::MaintenanceActivityRow;
use culprit_core::collected_resource_analysis::CollectedMaintenanceAnalysis;
use std::collections::BTreeSet;

pub fn maintenance(capture: &EvidenceCapture) -> Result<CollectedMaintenanceAnalysis> {
    let references = references(capture)?;
    let rows = capture
        .records()
        .iter()
        .zip(&references)
        .map(|(record, reference)| {
            let TelemetryRecord::Activity(observation) = &record.row().record else { return Err(invalid()) };
            Ok(MaintenanceActivityRow { observation, reference: Some(reference) })
        })
        .collect::<Result<Vec<_>>>()?;
    let report = culprit_checks::maintenance::explain(capture.source(), &rows, capture.is_read_complete())?;
    Ok(CollectedMaintenanceAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn references(capture: &EvidenceCapture) -> Result<Vec<DiagnosticEvidenceRef>> {
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
    capture
        .records()
        .iter()
        .map(|record| {
            let row = record.row();
            let ordinal = record.row_ordinal().checked_sub(1).ok_or_else(invalid)?;
            if row.capture != *capture.capture()
                || row.batch_id != record.batch_id()
                || !positions.insert((record.batch_id(), ordinal))
                || row.payload_sha256()? != record.native_payload_sha256()
            {
                return Err(invalid());
            }
            // The reader retains only exact reconstructed native rows. A truncated preview is a read gap,
            // not an altered Activity body to which an original receipt could be attached.
            DiagnosticEvidenceRef::from_export(capture.source(), capture.capture_manifest_sha256(), row, ordinal)
        })
        .collect()
}
