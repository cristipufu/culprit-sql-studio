//! A captured history read answers its finite question through the same offline queue and CAS owner.
use super::*;

pub(super) struct OriginalBasis {
    source: CollectionDescription,
    row: ExportedRecord,
}

pub(super) async fn parents(
    outbox: &mut Outbox,
    input: &DiagnosticInput,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<Option<Box<OriginalBasis>>> {
    let intent = input.investigation_intent()?.ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(DeliveryProblem::Integrity.error()) };
    for reference in intent.focus.references() {
        if resolve_evidence(outbox, &input.connection, reference, recovery, cancellation).await?.is_none() {
            return Ok(None);
        }
    }
    let (source, row) = outbox.diagnostic_evidence(&focus.basis).await?;
    let original = Box::new(OriginalBasis { source, row });
    if !intent.matches_parent_record(&original.source, &original.row) {
        return Err(DeliveryProblem::Integrity.error());
    }
    let Some(prerequisite) = focus.prerequisite.compilation_admission() else {
        return Ok(Some(original));
    };
    let context = outbox
        .diagnostic_input(&prerequisite.observation.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await?;
    let SourceObservation::FocusedCompilationAdmission { result, .. } = &context.source.observation else {
        return Err(DeliveryProblem::Integrity.error());
    };
    if !verify_focus_parent(outbox, &input.connection, &result.focus, recovery, cancellation).await? {
        return Ok(None);
    }
    let rows = context
        .rows
        .iter()
        .map(|row| match &row.row.record {
            TelemetryRecord::Operational(native) => Ok((&row.reference, native)),
            _ => Err(DeliveryProblem::Integrity.error()),
        })
        .collect::<Result<Vec<_>>>()?;
    if context.parent != prerequisite.observation.capture_ref()
        || !focus.matches_prerequisite_input(&original.source, &original.row, &context.source, rows)
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(Some(original))
}

pub(super) fn analyze(input: &DiagnosticInput, original: &OriginalBasis, at: &str) -> Result<Option<DiagnosticUpdate>> {
    if !matches!(input.source.observation, SourceObservation::SourceRead { .. })
        || !crate::outbox::source_read_retains_rows(&input.source, input.investigation.as_ref())?
            && !input.rows.is_empty()
    {
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
        SourceReadStageState::Answered { answer } if answer.result() == &input.parent => (),
        SourceReadStageState::Recovered => (),
        SourceReadStageState::Stopped { .. } => (),
        SourceReadStageState::Collecting => {
            let rows = input.rows.iter().map(|row| (&row.reference, &row.row)).collect::<Vec<_>>();
            let next = before.complete_source_read_from_originals(
                &intent,
                &original.source,
                &original.row,
                &input.source,
                &input.parent,
                &rows,
            )?;
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
        SourceReadStageState::Captured { .. } | SourceReadStageState::Answered { .. } => {
            return Err(DeliveryProblem::Integrity.error());
        }
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

#[cfg(test)]
mod storage_tests;
