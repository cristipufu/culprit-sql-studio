//! The original captured Activity frame is shared with page participation; it does not prove lock acquisition by SQL.
use super::*;
use culprit_core::collected_resource_analysis::CollectedMaintenanceAnalysis;

pub fn maintenance(capture: &EvidenceCapture) -> Result<CollectedMaintenanceAnalysis> {
    let activity = super::activity::CapturedActivity::new(capture)?;
    let report = culprit_checks::maintenance::explain(capture.source(), &activity.rows()?, capture.is_read_complete())?;
    Ok(CollectedMaintenanceAnalysis { context: crate::collected_context::from_capture(capture), report })
}
