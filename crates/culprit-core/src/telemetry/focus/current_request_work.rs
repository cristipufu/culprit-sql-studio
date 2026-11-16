//! Existing long-running activity can ask about its exact current native event, without inventing a new symptom.
use super::*;

impl ActivityRequestFocus {
    pub fn is_current_request_work_focus(&self) -> bool {
        self.is_valid()
            && self.target.engine() == crate::Engine::Mysql
            && self
                .original
                .key
                .request
                .as_ref()
                .and_then(|request| {
                    crate::engines::mysql_activity_statement_identity(&self.original.key.session, request)
                })
                .as_ref()
                == Some(&self.statement_identity)
    }
}

impl DiagnosticEpisode {
    pub fn matches_current_request_work_focus(&self, focus: &ActivityRequestFocus) -> bool {
        self.matches_request_origin(focus)
            && focus.is_current_request_work_focus()
            && focus.partition.native_epoch().is_some()
            && matches!(self.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
    }

    pub(super) fn plan_current_request_work_focus(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.analysis.needs_work()
            || self.scope.target.engine() != crate::Engine::Mysql
            || !matches!(self.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
        {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        let Some(focus) = self.original_request_focus(source, original, reference)? else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if !focus.is_current_request_work_focus() || focus.partition.native_epoch().is_none() {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        if !self.matches_current_request_work_focus(&focus) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::CurrentRequestWork { focus: Box::new(focus) })))
    }
}
