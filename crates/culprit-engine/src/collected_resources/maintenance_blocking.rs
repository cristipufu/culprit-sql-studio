//! Two authenticated captures keep independent contexts and canonical explanations, without native work.
use super::*;
use culprit_core::collected_resource_analysis::CollectedMaintenanceBlockingAnalysis;

pub fn maintenance_blocking(
    maintenance: &EvidenceCapture,
    blocking: &EvidenceCapture,
) -> Result<CollectedMaintenanceBlockingAnalysis> {
    let progress = super::maintenance_progress::CapturedMaintenance::new(maintenance)?;
    let focus = progress.snapshot.result.focus.as_deref().ok_or_else(invalid)?;
    if maintenance.capture() == blocking.capture()
        || maintenance.capture() == &focus.parent.capture
        || blocking.capture() == &focus.parent.capture
    {
        return Err(invalid());
    }
    let activity = super::activity::CapturedActivity::new(blocking)?;
    let frame = activity.frame()?;
    let observation = progress.observation_reference()?;
    let mut report = culprit_checks::maintenance_blocking::explain(
        culprit_checks::maintenance_blocking::MaintenanceProgressEvidence {
            source: maintenance.source(),
            snapshot: &progress.snapshot,
            observation: observation.as_ref(),
            read_complete: maintenance.is_read_complete(),
        },
        &frame,
    )
    .map_err(|_| invalid())?;
    progress.qualify_report(&mut report.maintenance);
    Ok(CollectedMaintenanceBlockingAnalysis {
        maintenance_context: crate::collected_context::from_capture(maintenance),
        blocking_context: crate::collected_context::from_capture(blocking),
        report,
    })
}
