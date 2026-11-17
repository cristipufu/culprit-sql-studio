//! Paired populations name their exact original frame in the authenticated source manifest before acquisition.
use super::*;

pub(crate) async fn resolve_original_activity(
    outbox: &mut Outbox,
    connection: &str,
    reference: &DiagnosticCaptureRef,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<Option<DiagnosticInput>> {
    if cancellation.is_cancelled() {
        return Ok(None);
    }
    if outbox.diagnostic_input_state(reference, DIAGNOSTIC_CONTROLLER_VERSION).await?
        == crate::outbox::DiagnosticInputState::Missing
    {
        let Some(recovery) = recovery else {
            return Err(Error::new(ErrorCode::NotFound, culprit_core::message!("collection.invalid_source"))
                .with_detail(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
        };
        if outbox.recovery_store_id() != Some(recovery.id()) {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let read = async {
            let advertised = recovery.capture_manifest(&reference.capture).await?;
            if diagnostic_recovery::capture_reference(&advertised)? != *reference {
                return Err(DeliveryProblem::Integrity.error());
            }
            let recovered = diagnostic_recovery::load_diagnostic_input(recovery, &advertised.range_id).await?;
            if recovered.range().manifest_digest() != advertised.digest()?
                || diagnostic_recovery::capture_reference(recovered.range().manifest())? != *reference
            {
                return Err(DeliveryProblem::Integrity.error());
            }
            Ok(recovered)
        };
        let Some(recovered) = read_recovery(read, cancellation).await? else { return Ok(None) };
        outbox.cache_diagnostic_evidence(connection, recovered).await?;
    }
    let original = outbox.diagnostic_input(&reference.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await?;
    if original.parent != *reference || !matches!(original.source.observation, SourceObservation::Activity(_)) {
        return Err(DeliveryProblem::Integrity.error());
    }
    original.activity_frame()?;
    Ok(Some(original))
}

pub(super) fn analyze(
    input: &DiagnosticInput,
    original: &DiagnosticInput,
    analyzed_at: &str,
) -> Result<DiagnosticUpdate> {
    let SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } = &input.source.observation else {
        return Err(CollectionStop::InvalidPage.error());
    };
    if original.parent != *activity {
        return Err(DeliveryProblem::Integrity.error());
    }
    let frame = original.activity_frame()?;
    let mut checkpoint = input.checkpoint.clone().unwrap_or_else(|| DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: input.source.target.clone(),
        input: input.parent.clone(),
        episodes: vec![],
    });
    let mut revisions = Vec::new();
    if input.rows.len() > MAX_DIAGNOSTIC_INPUTS {
        return Err(DeliveryProblem::Capacity.error());
    }
    for row in &input.rows {
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
            &row.row.record
        else {
            return Err(DeliveryProblem::Integrity.error());
        };
        if point.task_wait.is_none() {
            continue;
        }
        let update =
            observe_diagnostic_page_task(&input.source, point, &row.reference, &frame, Some(&checkpoint), analyzed_at)?;
        revisions.extend(update.revisions);
        checkpoint = update.checkpoint;
    }
    if let Some(intent) = input.investigation_intent()? {
        if !matches!(intent.focus, DiagnosticFocus::PageWaitTasks { .. })
            || !intent.matches_result_source(&input.source)
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        // Completing a bounded population read does not complete the original request's explanation. Only
        // actual task rows create their independent point episodes; missing tasks never resolve the trigger.
        if let Some(trigger) = checkpoint.episodes.iter_mut().find(|episode| intent.matches_episode(episode)) {
            let before = trigger.clone();
            trigger.analysis = if input.rows.iter().any(|row| {
                matches!(&row.row.record,
                TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. })
                    if page.task_wait.is_some())
            }) {
                DiagnosticAnalysisStatus::Partial
            } else {
                DiagnosticAnalysisStatus::Unresolved
            };
            trigger.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
            if let Some(mut revision) = DiagnosticRevision::between(Some(&before), trigger.clone(), analyzed_at)? {
                for reference in intent.focus.references() {
                    if !revision.inputs.contains(reference) {
                        revision.inputs.push(reference.clone());
                    }
                }
                revision.id = revision.expected_id()?;
                if !revision.is_valid() {
                    return Err(DeliveryProblem::Integrity.error());
                }
                *trigger = revision.episode.clone();
                revisions.push(revision);
            }
        }
    }
    checkpoint.input = input.parent.clone();
    if !checkpoint.is_valid() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let step_id = digest(
        &serde_json::to_vec(&(
            "paired-page-task-intake-v1",
            input.controller_version,
            &input.parent,
            activity,
            &input.checkpoint,
        ))
        .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    Ok(DiagnosticUpdate { step_id, analyzed_at: analyzed_at.into(), revisions, checkpoint })
}
