//! A durable reservation grants a bounded start window; waiting on disk cannot authorize a late native start.
use super::{FleetBackend, FleetSourceAdmission, scheduler::ADMISSION_START_WINDOW};
use crate::{
    capture::{CollectionAdmissionWindow, CollectionCancellation},
    outbox::Outbox,
};
use culprit_core::{Result, telemetry::*};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::time::Instant;

pub(super) struct AdmittedBackend<'a> {
    backend: &'a dyn FleetBackend,
    admitted_at: Instant,
    start_before: Instant,
    dispatched: AtomicBool,
    used_route: OnceLock<Option<String>>,
    capture: Mutex<AdmittedCapture>,
}
struct AdmittedCapture {
    source_work: SourceWorkState,
    attempt: Option<DiagnosticAttemptReceipt>,
}
impl<'a> AdmittedBackend<'a> {
    pub fn new(backend: &'a dyn FleetBackend, admitted_at: Instant) -> Self {
        Self {
            backend,
            admitted_at,
            start_before: admitted_at + ADMISSION_START_WINDOW,
            dispatched: AtomicBool::new(false),
            used_route: OnceLock::new(),
            capture: Mutex::new(AdmittedCapture { source_work: SourceWorkState::NotStarted, attempt: None }),
        }
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.start_before {
            return Err(CollectionStop::TimedOut.error());
        }
        if self.dispatched.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return Err(CollectionStop::QueryLimit.error());
        }
        Ok(())
    }
    fn context(
        &self,
        connection: &str,
        budget: &CollectionBudget,
        expected: Option<&str>,
    ) -> Result<FleetSourceAdmission> {
        self.check()?;
        let window = CollectionAdmissionWindow::new(self.admitted_at, budget)?;
        let route_revision = self.backend.route_revision(connection)?;
        if expected.is_some_and(|expected| route_revision.as_deref() != Some(expected)) {
            return Err(CollectionStop::SourceChanged.error());
        }
        window.check_start()?;
        self.used_route.set(route_revision.clone()).map_err(|_| CollectionStop::QueryLimit.error())?;
        Ok(FleetSourceAdmission { window, route_revision, investigation: None })
    }
    pub fn used_route_revision(&self) -> Option<&str> {
        self.used_route.get().and_then(|revision| revision.as_deref())
    }
    pub fn source_work(&self) -> SourceWorkState {
        self.capture.lock().map_or(SourceWorkState::Unconfirmed, |state| state.source_work)
    }
    pub fn attempt(&self) -> Result<Option<DiagnosticAttemptReceipt>> {
        self.capture
            .lock()
            .map(|state| state.attempt.clone())
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())
    }
    fn record_capture(&self, source_work: SourceWorkState, attempt: Option<DiagnosticAttemptReceipt>) -> Result<()> {
        *self.capture.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())? =
            AdmittedCapture { source_work, attempt };
        Ok(())
    }
    pub async fn discover(
        &self,
        connection: &str,
        request: &IntervalDiscoveryRequest,
    ) -> Result<IntervalDiscoveryPage> {
        let admission = self.context(connection, &request.budget, None)?;
        self.backend.discover(connection, request, admission).await
    }
    pub async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        let admission = self.context(connection, &request.budget, None)?;
        self.capture_admitted(connection, request, outbox, cancellation, admission).await
    }
    pub async fn capture_routed(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        expected_route: &str,
        intent: &DiagnosticInvestigationIntent,
    ) -> Result<CollectionManifest> {
        let mut admission = self.context(connection, &request.budget, Some(expected_route))?;
        if !intent.is_valid()
            || request.budget != intent.budget
            || !matches!(&request.selection, CollectionSelection::FocusedPageWait { focus } if focus.as_ref() == &intent.focus)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        admission.investigation = Some(intent.clone());
        self.capture_admitted(connection, request, outbox, cancellation, admission).await
    }
    async fn capture_admitted(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admission: FleetSourceAdmission,
    ) -> Result<CollectionManifest> {
        let expected = admission.investigation.as_ref().map(DiagnosticInvestigationIntent::key).transpose()?;
        self.record_capture(SourceWorkState::Active, None)?;
        let outcome = self.backend.capture(connection, request, outbox, cancellation, admission).await;
        let unconfirmed = matches!(outcome.source_work, SourceWorkState::Active | SourceWorkState::Unconfirmed)
            || outcome.result.is_ok() && outcome.source_work != SourceWorkState::Quiescent
            || outcome.result.as_ref().is_err_and(|error| {
                error.detail
                    == Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
            })
            || outcome.result.as_ref().is_ok_and(|manifest| {
                manifest.status == CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed)
            });
        self.record_capture(
            if unconfirmed { SourceWorkState::Unconfirmed } else { outcome.source_work },
            outcome.attempt.clone(),
        )?;
        if unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if outcome.attempt.as_ref().is_some_and(|attempt| !attempt.is_valid()) {
            return Err(DeliveryProblem::Integrity.error());
        }
        if let Ok(manifest) = &outcome.result {
            let actual = manifest.investigation_intent()?.map(|intent| intent.key()).transpose()?;
            if actual != expected || outcome.attempt.as_ref().is_some_and(|attempt| attempt.capture != manifest.capture)
            {
                return Err(DeliveryProblem::Integrity.error());
            }
        }
        if expected.is_some() && outcome.source_work == SourceWorkState::Quiescent && outcome.attempt.is_none() {
            return Err(DeliveryProblem::Integrity.error());
        }
        outcome.result
    }
}

#[cfg(test)]
mod tests;
