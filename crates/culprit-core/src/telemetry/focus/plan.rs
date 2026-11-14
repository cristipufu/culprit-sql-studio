//! Reconstruct one bounded question from retained native evidence, independently of episode lifecycle.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageWaitFocusPlan {
    Ready(Box<ActivityWaitFocus>),
    Unavailable(DiagnosticAnalysisIssue),
    NotRequired,
}

impl DiagnosticEpisode {
    /// Reconstruct a question from an original baseline row authenticated by the caller's outbox/recovery owner.
    /// Receipt derivation checks consistency here; it cannot prove manifest membership or authenticate storage.
    /// Native visibility may be partial, and a resolved episode may still owe analysis of its original evidence.
    pub fn plan_page_wait_focus(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<PageWaitFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        let DiagnosticSignal::PageWait { key, statement_identity, resource } = &self.signal else {
            return Ok(PageWaitFocusPlan::NotRequired);
        };
        if !self.analysis.needs_work() {
            return Ok(PageWaitFocusPlan::NotRequired);
        }
        let native = source.observation.activity().filter(|snapshot| snapshot.is_valid()).ok_or_else(invalid)?;
        DiagnosticActivityRow { reference, observation: original }.verify(source, &reference.capture_ref())?;
        let scope = DiagnosticScope::from_source(source)?;
        if *key != original.key
            || scope.source != self.scope.source
            || scope.target.telemetry_id()? != self.scope.target.telemetry_id()?
            || scope.population != self.scope.population
            || scope.measurement_scope != self.scope.measurement_scope
            || !ObservationBounds::new(&self.first_seen_at, &self.last_seen_at)
                .is_some_and(|bounds| bounds.contains(&native.at))
        {
            return Err(invalid());
        }
        let (Some(epoch), Some(statement)) = (&native.epoch, &original.statement_identity) else {
            return Ok(PageWaitFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if self.scope.native_epoch.is_none()
            || statement_identity.is_none()
            || original.scope != ActivityScopeBasis::RequestDatabase
            || original.wait.as_ref().and_then(|wait| wait.native_type.as_ref()).is_none()
        {
            return Ok(PageWaitFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        let focus = ActivityWaitFocus {
            parent: reference.clone(),
            target: source.target.clone(),
            source: source.source.clone(),
            epoch: epoch.clone(),
            original_observed_at: native.at.clone(),
            original: original.clone(),
            statement_identity: statement.clone(),
            resource: resource.clone(),
        };
        if !focus.matches_parent(source, original, reference) || !self.matches_focus(&focus) {
            return Err(invalid());
        }
        Ok(PageWaitFocusPlan::Ready(Box::new(focus)))
    }
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
