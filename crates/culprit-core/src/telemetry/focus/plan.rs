//! Reconstruct one bounded question from retained native evidence, independently of episode lifecycle.
use super::*;
mod writers;
pub use writers::{plan_page_wait_writer_focus, supports_page_wait_writers};
mod holders;
pub use holders::plan_page_task_holder_focus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageWaitFocusPlan {
    Ready(Box<ActivityWaitFocus>),
    Unavailable(DiagnosticAnalysisIssue),
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticFocusPlan {
    Ready(Box<DiagnosticFocus>),
    Unavailable(DiagnosticAnalysisIssue),
    NotRequired,
}

impl DiagnosticEpisode {
    pub fn plan_focus(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if self.source_read.is_some() || !self.analysis.needs_work() {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        if matches!(self.signal, DiagnosticSignal::LockWait { .. }) {
            return self.plan_blocking_focus(source, original, reference);
        }
        if matches!(self.signal, DiagnosticSignal::Activity { .. }) {
            return self.plan_current_request_work_focus(source, original, reference);
        }
        if matches!(self.signal, DiagnosticSignal::PageWait { .. }) {
            return Ok(match self.plan_page_wait_focus(source, original, reference)? {
                PageWaitFocusPlan::Ready(focus) => {
                    DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::PageWait { focus }))
                }
                PageWaitFocusPlan::Unavailable(issue) => DiagnosticFocusPlan::Unavailable(issue),
                PageWaitFocusPlan::NotRequired => DiagnosticFocusPlan::NotRequired,
            });
        }
        let (statement_identity, compilation) = match &self.signal {
            DiagnosticSignal::RequestMemory { statement_identity, .. } => (statement_identity, false),
            DiagnosticSignal::CompilationAdmission { statement_identity, .. } => (statement_identity, true),
            _ => return Ok(DiagnosticFocusPlan::NotRequired),
        };
        let Some(focus) = self.original_request_focus(source, original, reference)? else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if statement_identity.is_none()
            || focus.partition.native_epoch().is_none()
            || original.scope != ActivityScopeBasis::RequestDatabase
        {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        let question = if compilation {
            DiagnosticFocus::CompilationAdmission { focus: Box::new(focus) }
        } else {
            DiagnosticFocus::RequestMemory { focus: Box::new(focus) }
        };
        if !question.matches_parent(source, original, reference) || !self.matches_focus(&question) {
            return Err(invalid());
        }
        Ok(DiagnosticFocusPlan::Ready(Box::new(question)))
    }

    /// One authenticated original row supplies the common request envelope; each native question adds its own
    /// supported identity/scope conditions. A current reader cannot fill a missing original generation or token.
    pub(super) fn original_request_focus(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<Option<ActivityRequestFocus>> {
        let native = source.observation.activity().filter(|snapshot| snapshot.is_valid()).ok_or_else(invalid)?;
        DiagnosticActivityRow { reference, observation: original }.verify(source, &reference.capture_ref())?;
        let scope = DiagnosticScope::from_source(source)?;
        if self.signal.activity_key() != &original.key
            || scope.key()? != self.scope.key()?
            || !ObservationBounds::new(&self.first_seen_at, &self.last_seen_at)
                .is_some_and(|bounds| bounds.contains(&native.at))
        {
            return Err(invalid());
        }
        let (Some(epoch), Some(statement)) = (&native.epoch, &original.statement_identity) else { return Ok(None) };
        if !original.has_session_generation_authority()
            || !scope.partition.is_qualified()
            || !matches!(original.scope, ActivityScopeBasis::RequestDatabase | ActivityScopeBasis::SessionDatabase)
        {
            return Ok(None);
        }
        let focus = ActivityRequestFocus {
            parent: reference.clone(),
            target: source.target.clone(),
            partition: source.partition.clone(),
            database_writable: native.database_writable,
            source: source.source.clone(),
            epoch: epoch.clone(),
            original_observed_at: native.at.clone(),
            original: original.clone(),
            statement_identity: statement.clone(),
        };
        if !focus.matches_parent(source, original, reference) {
            return Err(invalid());
        }
        Ok(Some(focus))
    }

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
            || scope.partition != self.scope.partition
            || !ObservationBounds::new(&self.first_seen_at, &self.last_seen_at)
                .is_some_and(|bounds| bounds.contains(&native.at))
        {
            return Err(invalid());
        }
        let (Some(epoch), Some(statement)) = (&native.epoch, &original.statement_identity) else {
            return Ok(PageWaitFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if !original.has_session_generation_authority()
            || self.scope.native_epoch.is_none()
            || !scope.partition.is_qualified()
            || statement_identity.is_none()
            || original.scope != ActivityScopeBasis::RequestDatabase
            || original.wait.as_ref().and_then(|wait| wait.native_type.as_ref()).is_none()
        {
            return Ok(PageWaitFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        let focus = ActivityWaitFocus {
            participant: ActivityRequestFocus {
                parent: reference.clone(),
                target: source.target.clone(),
                partition: source.partition.clone(),
                database_writable: native.database_writable,
                source: source.source.clone(),
                epoch: epoch.clone(),
                original_observed_at: native.at.clone(),
                original: original.clone(),
                statement_identity: statement.clone(),
            },
            resource: resource.clone(),
        };
        if !focus.matches_parent(source, original, reference) || !self.matches_page_wait_focus(&focus) {
            return Err(invalid());
        }
        Ok(PageWaitFocusPlan::Ready(Box::new(focus)))
    }
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

/// Receipt consistency over the exact canonical page row. Its caller authenticates the manifest/storage;
/// serialization alone cannot grant a native member, original role or a durable capture revision.
fn verify_page_reference(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    reference: &DiagnosticEvidenceRef,
) -> Result<()> {
    if !page.fits_source_scope(source)
        || !source.observation.snapshot().is_some_and(|snapshot| reference.row_ordinal < snapshot.records)
    {
        return Err(invalid());
    }
    let row = OperationalObservation {
        source: page.source.clone(),
        key: page.key()?,
        fact: OperationalFact::PageWait(Box::new(page.clone())),
    };
    reference.validate_canonical_record(source, &TelemetryRecord::Operational(row))
}
