//! A native lock symptom asks about its original request, not about whichever request later reuses its PID.
use super::*;

impl DiagnosticEpisode {
    pub fn matches_blocking_focus(&self, focus: &ActivityRequestFocus) -> bool {
        self.matches_request_origin(focus)
            && focus.is_blocking_focus()
            && matches!(self.signal, DiagnosticSignal::LockWait { .. })
    }

    pub(super) fn plan_blocking_focus(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.analysis.needs_work() || !matches!(self.signal, DiagnosticSignal::LockWait { .. }) {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        let Some(focus) = self.original_request_focus(source, original, reference)? else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if !focus.is_blocking_focus() {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        if !self.matches_blocking_focus(&focus) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::Blocking { focus: Box::new(focus) })))
    }
}
