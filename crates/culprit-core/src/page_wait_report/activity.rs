//! Original Activity bodies qualify recorded request context independently from later task rechecks.
use crate::{Result, telemetry::*};

#[derive(Clone, Copy)]
pub enum PageWaitActivityEvidence<'a> {
    Focused {
        source: &'a CollectionDescription,
        original: &'a ActivityRequestFocus,
        frame: &'a ActivityFrameEvidence<'a>,
    },
    Tasks {
        source: &'a CollectionDescription,
        frame: &'a ActivityFrameEvidence<'a>,
    },
}

impl<'a> PageWaitActivityEvidence<'a> {
    /// A declared companion is an intake selector, never authentication of its header or body.
    pub fn declared_parent(source: &CollectionDescription) -> Option<DiagnosticCaptureRef> {
        match &source.observation {
            SourceObservation::FocusedPageWait { result, .. } => Some(result.focus.participant().parent.capture_ref()),
            SourceObservation::PageWaitTargets { original_activity, .. } => original_activity.clone(),
            _ => None,
        }
    }

    pub fn frame(self) -> &'a ActivityFrameEvidence<'a> {
        match self {
            Self::Focused { frame, .. } | Self::Tasks { frame, .. } => frame,
        }
    }

    /// Storage authenticates the supplied capture, including an empty or partial body, before this comparison.
    pub fn validate(self) -> Result<()> {
        match self {
            Self::Focused { source, original, frame } => validate_focused(source, original, frame),
            Self::Tasks { source, frame } => validate_tasks(source, frame),
        }
    }

    pub fn recorded_original(self) -> Result<Option<ActivityFrameRow<'a>>> {
        self.validate()?;
        let Self::Focused { original, .. } = self else { return Ok(None) };
        Ok(self.recorded_row(&original.original.key))
    }

    /// Only the initial native task point can bind earlier SQL. Later successful checks grant no fallback.
    pub fn recorded_task_request(self, page: &PageWaitObservation) -> Result<Option<ActivityFrameRow<'a>>> {
        self.validate()?;
        let Self::Tasks { source, .. } = self else { return Ok(None) };
        if !page.fits_source_scope(source) {
            return Err(CollectionStop::InvalidPage.error());
        }
        if page.task_wait.is_none()
            || page.request.scope != ActivityScopeBasis::RequestDatabase
            || !page.request.has_session_generation_authority()
        {
            return Ok(None);
        }
        let Some(row) = self.recorded_row(&page.request.key) else { return Ok(None) };
        if row.observation.scope != ActivityScopeBasis::RequestDatabase
            || row.observation.statement_identity.is_none()
            || row.observation.statement_identity != page.request.statement_identity
        {
            return Ok(None);
        }
        Ok(Some(row))
    }

    fn recorded_row(self, key: &ActivityKey) -> Option<ActivityFrameRow<'a>> {
        let frame = self.frame();
        if !frame.read_complete()
            || !frame.native().coverage.complete_scope
            || !frame.context().has_qualified_local_owner()
            || !frame.has_session_generation_authority(&key.session)
        {
            return None;
        }
        frame.row(key).filter(|row| row.reference.is_some())
    }
}

fn validate_tasks(source: &CollectionDescription, frame: &ActivityFrameEvidence<'_>) -> Result<()> {
    let SourceObservation::PageWaitTargets { snapshot, original_activity: Some(original), .. } = &source.observation
    else {
        return Err(CollectionStop::InvalidPage.error());
    };
    let context = NativeReadContext::from_snapshot(source)?;
    if !original.is_valid()
        || snapshot.dataset != SnapshotDataset::PageWaitTargets
        || source.target.engine() != crate::Engine::Mssql
        || source.target.database().is_none()
        || !source.observation.covers_request()
        || frame.source().captured_range()?.scope_id != original.source_scope_id
        || frame.capture_ref().is_some_and(|actual| &actual != original)
        || !context.matches_earlier_scope(&frame.context())
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(())
}

fn validate_focused(
    source: &CollectionDescription,
    original: &ActivityRequestFocus,
    frame: &ActivityFrameEvidence<'_>,
) -> Result<()> {
    let snapshot = source.observation.snapshot().ok_or_else(|| CollectionStop::InvalidPage.error())?;
    if snapshot.dataset != SnapshotDataset::PageWaitTargets
        || original.original.scope != ActivityScopeBasis::RequestDatabase
        || !source.observation.covers_request()
        || !original.matches_capture_scope(source)
        || ObservationBounds::new(&original.original_observed_at, &snapshot.observed_until).is_none()
        || !original.matches_parent(frame.source(), &original.original, &original.parent)
        || frame.capture_ref().is_some_and(|capture| capture != original.parent.capture_ref())
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    if let SourceObservation::FocusedPageWait { result, .. } = &source.observation {
        let bounds = ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
            .ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if result.focus.participant() != original
            || !bounds.contains(&result.observed_from)
            || !bounds.contains(&result.observed_until)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
    }
    match frame.row(&original.original.key) {
        Some(row)
            if !row
                .reference
                .is_some_and(|reference| original.matches_parent(frame.source(), row.observation, reference)) =>
        {
            Err(CollectionStop::InvalidPage.error())
        }
        None if frame.read_complete() => Err(CollectionStop::InvalidPage.error()),
        Some(_) => Ok(()),
        None => validate_missing_original_copy(original, frame),
    }
}

fn validate_missing_original_copy(original: &ActivityRequestFocus, frame: &ActivityFrameEvidence<'_>) -> Result<()> {
    let reference = &original.parent;
    if reference.row_ordinal >= frame.native().observations {
        return Err(CollectionStop::InvalidPage.error());
    }
    // Only check that the supplied copy is consistent with its declared receipt. It does not become a native
    // observation, authenticate an unread position, or permit owner context when the original row is absent.
    reference.validate_canonical_record(frame.source(), &TelemetryRecord::from(original.original.clone()))
}
