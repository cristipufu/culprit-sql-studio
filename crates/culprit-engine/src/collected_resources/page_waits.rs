//! Capture-local page proof remains independent from later requester/wait changes. Original waiters are not owners.
use super::*;
use culprit_core::PageWaitActivityEvidence;
use culprit_core::collected_resource_analysis::CollectedPageWaitAnalysis;

pub fn page_waits(capture: &EvidenceCapture, activity: Option<&EvidenceCapture>) -> Result<CollectedPageWaitAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::PageWaitTargets)?;
    if let SourceObservation::FocusedPageWait { result, .. } = &capture.source().observation
        && !result.focus.matches_capture_scope(capture.source())
    {
        return Err(invalid());
    }
    if let Some(parent) = activity {
        let original = PageWaitActivityEvidence::declared_parent(capture.source())
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
        if parent.capture() != &original.capture
            || parent.capture_manifest_sha256() != original.manifest_sha256
            || parent.source().captured_range()?.scope_id != original.source_scope_id
        {
            return Err(invalid());
        }
    }
    let parent = activity.map(super::activity::CapturedActivity::new).transpose()?;
    let frame = parent.as_ref().map(super::activity::CapturedActivity::frame).transpose()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::PageWait(observation) = &row.fact else { return Err(invalid()) };
            // The authenticated source binds shared authority, original request scope and native bounds together.
            if !observation.fits_source_scope(capture.source()) {
                return Err(invalid());
            }
            if let SourceObservation::FocusedPageWait { result, .. } = &capture.source().observation
                && !result.accepts_observation(observation)
            {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let participation = frame.as_ref().map(|frame| match &capture.source().observation {
        SourceObservation::FocusedPageWait { result, .. } => {
            PageWaitActivityEvidence::Focused { source: capture.source(), original: result.focus.participant(), frame }
        }
        _ => PageWaitActivityEvidence::Tasks { source: capture.source(), frame },
    });
    let report = culprit_checks::page_waits::explain(&observations, Some(&source), participation)?;
    Ok(CollectedPageWaitAnalysis {
        context: crate::collected_context::from_capture(capture),
        activity_context: activity.map(crate::collected_context::from_capture),
        report,
    })
}
