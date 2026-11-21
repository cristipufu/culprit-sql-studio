//! Engine-only capability for the existing gate/factory boundary; provider factories receive only its context.
use super::*;
pub struct ProviderOperationObservation {
    pub(super) context: ProviderReadContext,
}
impl ProviderOperationObservation {
    pub fn unobserved() -> Self {
        Self { context: ProviderReadContext::unobserved() }
    }
    pub fn context(&self) -> ProviderReadContext {
        self.context.clone()
    }
    pub fn is_observed(&self) -> bool {
        self.context.is_observed()
    }
    pub fn observation_id(&self) -> Option<String> {
        Some(locked(self.context.0.as_ref()?).id.clone())
    }
    pub fn require_kind(&self, kind: ProviderReadKind) -> Result<()> {
        if self.context.0.as_ref().is_some_and(|state| locked(state).kind != kind) {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn require_open(&self) -> Result<()> {
        if self.context.0.as_ref().is_some_and(|state| {
            let state = locked(state);
            state.closed || state.admitted
        }) {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn bind_route(&self, route: SourceAdmissionRoute) {
        self.context.update(|state| {
            let already_bound = state.route.as_ref().is_some_and(|before| before != &route);
            if !route.matches_observation(&state.id) || already_bound {
                state.gap(ProviderReadGap::InvalidObservation);
            } else {
                state.route = Some(route);
            }
        });
    }
    pub fn bind_engine(&self, engine: Engine) {
        self.context.update(|state| {
            if state.engine.is_some_and(|before| before != engine) {
                state.gap(ProviderReadGap::InvalidObservation);
            } else {
                state.engine = Some(engine);
            }
        });
    }
    pub fn admitted(&self) {
        self.context.update(|state| state.admitted = true);
    }
    pub fn native_started(&self) {
        self.context.update(|state| state.native_started = true);
    }
    /// Consumes the one engine capability. Providers cannot supply an earlier/competing Quiet result.
    pub fn source_finished<T>(
        self,
        original: &Result<T>,
        disposition: ProviderReadDisposition,
        finalization: Result<()>,
    ) -> Option<ProviderAcquisitionSummary> {
        let state = self.context.0.as_ref()?;
        let mut state = locked(state);
        if state.closed {
            return None;
        }
        if state.settlement.is_some() {
            state.gap(ProviderReadGap::InvalidObservation);
            state.settlement =
                Some((ProviderReadDisposition::Unconfirmed, Err(CollectionStop::CancellationUnconfirmed.error())));
        } else {
            state.settlement = Some((disposition, finalization));
        }
        Some(freeze(&mut state, original.as_ref().err().map(ProviderReadFailure::from_error)))
    }
    pub fn factory(&self) -> ProviderFactoryObservation {
        let began = now();
        let mut active = false;
        self.context.update(|state| {
            if state.factory.is_some() || state.native_started {
                state.gap(ProviderReadGap::InvalidObservation);
            } else {
                active = true;
                state.factory = Some(ProviderFactorySpan {
                    client_started_at: began,
                    client_finished_at: None,
                    client_elapsed_ms: None,
                    outcome: ProviderFactoryOutcome::Pending,
                });
            }
        });
        ProviderFactoryObservation { context: self.context.clone(), started: Instant::now(), active, finished: false }
    }
}
pub struct ProviderFactoryObservation {
    context: ProviderReadContext,
    started: Instant,
    active: bool,
    finished: bool,
}
impl ProviderFactoryObservation {
    pub fn finish<T>(mut self, result: &Result<T>) {
        let outcome = match result {
            Ok(_) => ProviderFactoryOutcome::Constructed,
            Err(error) => ProviderFactoryOutcome::Rejected { code: error.code },
        };
        self.record(outcome);
        self.finished = true;
    }
    fn record(&self, outcome: ProviderFactoryOutcome) {
        if !self.active {
            return;
        }
        self.context.update(|state| {
            if let Some(factory) = &mut state.factory {
                if factory.outcome != ProviderFactoryOutcome::Pending {
                    state.gap(ProviderReadGap::InvalidObservation);
                    return;
                }
                factory.client_finished_at = Some(now());
                factory.client_elapsed_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
                factory.outcome = outcome;
            }
        });
    }
}
impl Drop for ProviderFactoryObservation {
    fn drop(&mut self) {
        if !self.finished {
            self.record(ProviderFactoryOutcome::Abandoned);
        }
    }
}
