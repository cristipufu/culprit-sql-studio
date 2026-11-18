//! An explicit task question uses its original page receipt and independently qualified ORIGINAL Activity.
//! It does not create a coordinator wait, update an incident or acquire any current database information.
use super::*;

pub fn plan_page_task_holder_focus(
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
    reference: &DiagnosticEvidenceRef,
    frame: &ActivityFrameEvidence<'_>,
) -> Result<DiagnosticFocusPlan> {
    verify_page_reference(page_source, page, reference)?;
    let Some(task) = &page.task_wait else { return Ok(DiagnosticFocusPlan::NotRequired) };
    if task.resource.kind != WaitResourceKind::MemoryPage
        || page_source.target.database().is_none()
        || page.resolution != PageWaitResolution::ObservedAtBothBounds
        || page.target.as_ref().is_none_or(|target| target.writer_target_key().is_err())
    {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported));
    }
    if !frame.read_complete() {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable));
    }
    if !frame.context().has_qualified_local_owner()
        || !page_source.is_partition_qualified()
        || !matches!(page_source.partition, SourcePartition::InstanceLocal { .. })
    {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
    }
    let page_context = NativeReadContext::from_snapshot(page_source)?;
    if frame.native().database_writable.is_none() || page_context.database_writable.is_none() {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable));
    }
    if frame.source().target != page_source.target
        || frame.source().partition != page_source.partition
        || frame.native().database_writable != page_context.database_writable
        || ObservationBounds::new(&frame.native().observed_until, &page.observed_from).is_none()
    {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::SourceChanged));
    }
    let Some(original) = frame.row(&page.request.key) else {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
    };
    if !frame.has_session_generation_authority(&original.observation.key.session) {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
    }
    let (Some(parent), Some(statement), Some(epoch)) =
        (original.reference, &original.observation.statement_identity, &frame.native().epoch)
    else {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
    };
    if let SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } = &page_source.observation
        && (*activity != parent.capture_ref() || frame.source().captured_range()?.scope_id != activity.source_scope_id)
    {
        return Err(invalid());
    }
    if page.statement_identity.is_none() {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
    }
    if original.observation.scope != ActivityScopeBasis::RequestDatabase
        || page.statement_identity.as_ref() != Some(statement)
    {
        return Err(invalid());
    }
    let focus = PageTaskHolderFocus {
        participant: ActivityRequestFocus {
            parent: parent.clone(),
            target: frame.source().target.clone(),
            partition: frame.source().partition.clone(),
            database_writable: frame.native().database_writable,
            source: frame.source().source.clone(),
            epoch: epoch.clone(),
            original_observed_at: frame.native().at.clone(),
            original: original.observation.clone(),
            statement_identity: statement.clone(),
        },
        page: reference.clone(),
        address: page.observed_address.ok_or_else(invalid)?,
        task: task.clone(),
        target: page.target.clone().ok_or_else(invalid)?,
        original_observed_until: page.observed_until.clone(),
    };
    if !focus.matches_page(page_source, page, reference) || !focus.matches_activity_frame(frame) {
        return Err(invalid());
    }
    Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::PageTaskHolders { focus: Box::new(focus) })))
}
