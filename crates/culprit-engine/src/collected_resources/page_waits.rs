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
    let original = match (activity, &capture.source().observation) {
        (Some(parent), SourceObservation::FocusedPageWait { result, .. }) => {
            let focus = result.focus.participant();
            if parent.capture() != &focus.parent.capture
                || parent.capture_manifest_sha256() != focus.parent.manifest_sha256
                || parent.source().captured_range()?.scope_id != focus.parent.source_scope_id
            {
                return Err(invalid());
            }
            Some(focus)
        }
        (Some(_), _) => return Err(DeliveryProblem::UnsupportedSchema.error()),
        (None, _) => None,
    };
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
    let participation = original.zip(frame.as_ref()).map(|(original, frame)| PageWaitActivityEvidence {
        source: capture.source(),
        original,
        frame,
    });
    let report = culprit_checks::page_waits::explain(&observations, Some(&source), participation)?;
    Ok(CollectedPageWaitAnalysis {
        context: crate::collected_context::from_capture(capture),
        activity_context: activity.map(crate::collected_context::from_capture),
        report,
    })
}
