use super::*;
use culprit_core::collected_resource_analysis::CollectedSessionCensusAnalysis;

/// The existing Activity owner authenticates the original header, positions and every retained row once.
pub fn session_census(capture: &EvidenceCapture) -> Result<CollectedSessionCensusAnalysis> {
    let original = activity::CapturedActivity::new(capture)?;
    let frame = original.frame()?;
    Ok(CollectedSessionCensusAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(culprit_checks::session_census::explain_frame(&frame)?),
    })
}
