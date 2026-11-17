//! A captured history read answers its finite question through the same offline queue and CAS owner.
use super::*;

pub(super) async fn parents(
    outbox: &mut Outbox,
    input: &DiagnosticInput,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<bool> {
    let intent = input.investigation_intent()?.ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(DeliveryProblem::Integrity.error()) };
    for reference in intent.focus.references() {
        if resolve_evidence(outbox, &input.connection, reference, recovery, cancellation).await?.is_none() {
            return Ok(false);
        }
    }
    let (source, original) = outbox.diagnostic_evidence(&focus.basis).await?;
    if !intent.matches_parent_record(&source, &original) {
        return Err(DeliveryProblem::Integrity.error());
    }
    let context = outbox
        .diagnostic_input(&focus.prerequisite.observation.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await?;
    let SourceObservation::FocusedCompilationAdmission { result, .. } = &context.source.observation else {
        return Err(DeliveryProblem::Integrity.error());
    };
    if !verify_focus_parent(outbox, &input.connection, &result.focus, recovery, cancellation).await? {
        return Ok(false);
    }
    let rows = context
        .rows
        .iter()
        .map(|row| match &row.row.record {
            TelemetryRecord::Operational(native) => Ok((&row.reference, native)),
            _ => Err(DeliveryProblem::Integrity.error()),
        })
        .collect::<Result<Vec<_>>>()?;
    if context.parent != focus.prerequisite.observation.capture_ref()
        || !focus.matches_prerequisite_input(&source, &original, &context.source, rows)
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(true)
}

pub(super) fn analyze(input: &DiagnosticInput, at: &str) -> Result<Option<DiagnosticUpdate>> {
    if !matches!(input.source.observation, SourceObservation::SourceRead { .. }) || !input.rows.is_empty() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let intent = input.investigation_intent()?.ok_or_else(|| DeliveryProblem::Integrity.error())?;
    if !intent.matches_result_source(&input.source) {
        return Err(DeliveryProblem::Integrity.error());
    }
    let Some(mut checkpoint) = input.checkpoint.clone() else { return Ok(None) };
    let Some(index) = checkpoint.episodes.iter().position(|episode| episode.id == intent.episode_id) else {
        return Ok(None);
    };
    let before = checkpoint.episodes[index].clone();
    let stage = before.source_read.as_deref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let mut revisions = Vec::new();
    match &stage.state {
        SourceReadStageState::Captured { capture } if capture == &input.parent => (),
        SourceReadStageState::Stopped { .. } => (),
        SourceReadStageState::Collecting => {
            let next = before.complete_source_read(&intent, &input.source, &input.parent)?;
            let event = DiagnosticInvestigationEvent {
                intent: Box::new(intent),
                attempt: Some(DiagnosticAttemptReceipt {
                    capture: input.parent.capture.clone(),
                    manifest_sha256: Some(input.parent.manifest_sha256.clone()),
                }),
                source_work: SourceWorkState::Quiescent,
                outcome: DiagnosticInvestigationOutcome::Captured { capture: input.parent.clone() },
            };
            let revision = DiagnosticRevision::with_investigation(&before, next, event, at)?
                .ok_or_else(|| DeliveryProblem::Integrity.error())?;
            checkpoint.episodes[index] = revision.episode.clone();
            revisions.push(revision);
        }
        // A missing earlier immutable admission/history head can arrive after the result range. It cannot be
        // reconstructed from a current same-ID actor or promoted from the captured data rows.
        SourceReadStageState::Pending | SourceReadStageState::Deferred { .. } => return Ok(None),
        SourceReadStageState::Captured { .. } => return Err(DeliveryProblem::Integrity.error()),
    }
    checkpoint.input = input.parent.clone();
    checkpoint.episodes.retain(DiagnosticEpisode::retained_in_checkpoint);
    Ok(Some(DiagnosticUpdate {
        step_id: digest(
            &serde_json::to_vec(&("diagnostic-source-read-control-v1", &input.parent, &revisions))
                .map_err(|_| DeliveryProblem::Integrity.error())?,
        ),
        analyzed_at: at.into(),
        revisions,
        checkpoint,
    }))
}
