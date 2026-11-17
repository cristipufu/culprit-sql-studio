use super::*;
use crate::collection_files::ObservationFiles;

// A revision retains the raw roster both as report references and as its immutable input inventory.
const DIAGNOSTIC_PAGE_BYTES: u32 = 512 * 1024;

#[cfg(test)]
mod limits;

struct StoredDiagnosticStep {
    id: String,
    update_digest: String,
    result: Option<String>,
}

fn local_budget() -> CollectionBudget {
    CollectionBudget {
        page_rows: 128,
        max_snapshot_rows: (MAX_DIAGNOSTIC_EPISODES * 2) as u32,
        page_bytes: DIAGNOSTIC_PAGE_BYTES,
        total_bytes: 16 * 1024 * 1024,
        max_pages: 128,
        max_queries: 1,
        ..Default::default()
    }
}

impl Files {
    pub(in crate::outbox) fn commit_diagnostics(
        &mut self,
        input: DiagnosticInput,
        update: DiagnosticUpdate,
    ) -> Result<CaptureIdentity> {
        if !self.recovered || !self.journal.unfinished()?.is_empty() {
            self.recover()?;
        }
        let existing: Option<StoredDiagnosticStep> = self
            .journal
            .conn
            .query_row(
                "SELECT observation_id,update_digest,result_json FROM diagnostic_steps WHERE input_id=?1 AND step_id=?2",
                params![input.parent.capture.observation_id, update.step_id],
                |row| Ok(StoredDiagnosticStep { id: row.get(0)?, update_digest: row.get(1)?, result: row.get(2)? }),
            )
            .optional()
            .map_err(storage)?;
        if let Some(stored) = existing {
            if !valid_digest(&stored.update_digest) || stored.update_digest != update.replay_digest()? {
                return Err(DeliveryProblem::Conflict.error());
            }
            let id = stored.id;
            if let Some(result) = stored.result {
                let receipt: DiagnosticCaptureRef = decode(&result)?;
                if !receipt.is_valid() || receipt.capture.observation_id != id {
                    return Err(integrity());
                }
                return Ok(receipt.capture);
            }
            let capture = self.journal.capture(&id)?;
            if capture.state == OutboxCaptureState::Ready {
                self.validated(&id)?;
                return Ok(capture.identity);
            }
            if capture.state == OutboxCaptureState::Capturing {
                return Err(problem(OutboxProblem::RecoveryRequired));
            }
            self.journal
                .conn
                .execute("DELETE FROM diagnostic_steps WHERE observation_id=?1", [&id])
                .map_err(storage)?;
        }
        self.validate_diagnostic_update(&input, &update)?;
        let controller_target = input.controller_target()?.clone();
        let budget = local_budget();
        let parts = checkpoint_parts(&update.checkpoint, &budget)?;
        let request = CollectionRequest {
            subject: match &controller_target {
                CollectionTarget::Database(_) => CollectionSubject::Database,
                CollectionTarget::Shared(resource) => CollectionSubject::Shared { resource: resource.clone() },
            },
            selection: CollectionSelection::Diagnostics {
                parent: input.parent.clone(),
                controller_version: input.controller_version,
                step_id: update.step_id.clone(),
            },
            budget,
        };
        let reservation = self.reserve(&input.connection, &request)?;
        let identity = reservation.entry.identity.clone();
        let source = CollectionDescription {
            partition: culprit_core::telemetry::SourcePartition::Target,
            target: controller_target,
            source: DIAGNOSTICS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
                parent: input.parent.clone(),
                controller_version: input.controller_version,
                step_id: update.step_id.clone(),
                analyzed_at: update.analyzed_at.clone(),
                records: update.revisions.len() as u32,
                checkpoint_records: parts.len() as u32,
                scope_bindings: update
                    .checkpoint
                    .episodes
                    .iter()
                    .map(|episode| &episode.scope)
                    .chain(update.revisions.iter().map(|revision| &revision.episode.scope))
                    .map(|scope| Ok((scope.key()?, scope.clone())))
                    .collect::<Result<_>>()?,
            }),
        };
        let key = update.checkpoint.key()?;
        self.journal
            .conn
            .execute(
                "INSERT INTO diagnostic_steps(observation_id,input_id,controller_key,expected_head,step_id,update_json,update_digest,result_json,archived) VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,0)",
                params![
                    identity.observation_id,
                    input.parent.capture.observation_id,
                    key,
                    input.expected_head,
                    update.step_id,
                    encode(&update)?,
                    update.replay_digest()?
                ],
            )
            .map_err(storage)?;
        let mut files = ObservationFiles::create(&reservation.directory, request, identity.clone(), None)?;
        files.describe(source)?;
        let records = update
            .revisions
            .iter()
            .map(|revision| {
                TelemetryRecord::Operational(OperationalObservation {
                    source: DIAGNOSTICS_SOURCE.into(),
                    key: revision.id.clone(),
                    fact: OperationalFact::DiagnosticRevision(Box::new(revision.clone())),
                })
            })
            .collect::<Vec<_>>();
        write_stream(&mut files, 0, &records)?;
        write_stream(&mut files, 1, &parts.into_iter().map(TelemetryRecord::DiagnosticCheckpoint).collect::<Vec<_>>())?;
        files.confirm_source(SourceCompletion::Verified)?;
        let manifest = files.finish(0)?;
        if manifest.status != CollectionStatus::Complete {
            return Err(problem(OutboxProblem::Integrity));
        }
        self.finish(reservation)?;
        Ok(identity)
    }

    fn validate_diagnostic_update(&mut self, input: &DiagnosticInput, update: &DiagnosticUpdate) -> Result<()> {
        let current = self.diagnostic_input(&input.parent.capture.observation_id, input.controller_version)?;
        if current.parent != input.parent
            || current.source != input.source
            || current.expected_head != input.expected_head
            || current.checkpoint != input.checkpoint
            || encode_investigation(current.investigation.as_ref())?
                != encode_investigation(input.investigation.as_ref())?
        {
            return Err(DeliveryProblem::Conflict.error());
        }
        let acknowledged: bool = self
            .journal
            .conn
            .query_row(
                "SELECT acknowledged FROM diagnostic_inputs WHERE observation_id=?1 AND controller_version=?2",
                params![input.parent.capture.observation_id, input.controller_version],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if acknowledged
            && (update.revisions.is_empty()
                || update.revisions.iter().any(|revision| revision.previous_revision.is_none()))
        {
            // Retained evidence can support a later CAS transition, but cannot replay baseline admission.
            return Err(DeliveryProblem::Conflict.error());
        }
        if !valid_digest(&update.step_id)
            || canonical_time(&update.analyzed_at).is_err()
            || !update.checkpoint.is_valid()
            || update.checkpoint.input != input.parent
            || update.checkpoint.target != *current.controller_target()?
            || update.checkpoint.controller_version != input.controller_version
            || update.revisions.len() > MAX_DIAGNOSTIC_EPISODES
            || encode(update)?.len() > 2 * MAX_CHECKPOINT_BYTES
        {
            return Err(integrity());
        }
        super::journal::validate_transition(&self.journal.conn, &update.checkpoint.key()?, update)?;
        let intent = current.investigation_intent()?;
        if let Some(intent) = &intent {
            self.verify_investigation_intent(intent)?;
        }
        let mut known: BTreeSet<String> = current.rows.iter().map(|row| row.reference.key()).collect::<Result<_>>()?;
        if let Some(previous) = &current.checkpoint {
            for reference in super::journal::checkpoint_references(previous) {
                known.insert(reference.key()?);
            }
        }
        let paired_original = if let SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } =
            &current.source.observation
        {
            Some(self.verified_page_task_activity(activity)?.0)
        } else {
            None
        };
        let original_frame = paired_original.as_ref().map(DiagnosticInput::activity_frame).transpose()?;
        for revision in &update.revisions {
            if let Some(event) = &revision.investigation {
                self.validate_investigation_event(event)?;
            }
            if revision.previous_revision.is_none() {
                validate_new_episode(&current, &revision.episode, original_frame.as_ref())?;
            }
            if !revision.is_valid()
                || revision.controller_version != input.controller_version
                || revision.analyzed_at != update.analyzed_at
                || revision.episode.scope.target.telemetry_id()? != current.controller_target()?.telemetry_id()?
                || intent.as_ref().is_some_and(|intent| !intent.permits_result_episode(&revision.episode))
            {
                return Err(integrity());
            }
            if let Some(intent) = &intent
                && let DiagnosticExecutionBinding::Shared { owner } = &intent.execution
                && (!revision.inputs.contains(&intent.focus.participant().parent) || !revision.inputs.contains(owner))
            {
                return Err(integrity());
            }
            for reference in &revision.inputs {
                if !known.contains(&reference.key()?) {
                    let (source, row) = self.diagnostic_reference(reference)?;
                    let shared_intent =
                        revision.investigation.as_ref().map(|event| event.intent.as_ref()).or(intent.as_ref());
                    let shared_owner = shared_intent.is_some_and(|intent| {
                        intent.matches_episode(&revision.episode)
                            && matches!(&intent.execution, DiagnosticExecutionBinding::Shared { owner } if owner.as_ref() == reference)
                            && intent.matches_shared_owner(&source, &row)
                    });
                    if source.target.telemetry_id()? != revision.episode.scope.target.telemetry_id()? && !shared_owner {
                        return Err(integrity());
                    }
                    known.insert(reference.key()?);
                }
            }
            for reference in
                [&revision.episode.first, &revision.episode.latest].into_iter().chain(revision.episode.peak.iter())
            {
                if !known.contains(&reference.key()?) {
                    return Err(integrity());
                }
            }
            if let Some(report) = &revision.episode.report {
                let previous_report = current
                    .checkpoint
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.episodes.iter().find(|episode| episode.id == revision.episode.id))
                    .and_then(|episode| episode.report.as_ref());
                if previous_report != Some(report) {
                    let (source, row) = self.diagnostic_reference(report.evidence())?;
                    if !report.matches_observation(&revision.episode, &source, &row) {
                        return Err(integrity());
                    }
                    let focus = source.observation.focus().ok_or_else(integrity)?;
                    match &source.observation {
                        SourceObservation::PageWaitWriters { .. } => (),
                        SourceObservation::PageTaskHolders { focus, .. } => {
                            self.verify_page_task_holder_focus(focus)?
                        }
                        _ => self.verify_focus_parent(focus)?,
                    }
                    let report_input =
                        self.diagnostic_input(&report.evidence().capture.observation_id, input.controller_version)?;
                    let report_intent = report_input.investigation_intent()?;
                    if report_input.parent != report.evidence().capture_ref()
                        || report_input.source != source
                        || report_input.controller_target()?.telemetry_id()?
                            != revision.episode.scope.target.telemetry_id()?
                        || report_intent.as_ref().is_some_and(|intent| !intent.matches_episode(&revision.episode))
                        || !revision.inputs.contains(&focus.parent)
                    {
                        return Err(integrity());
                    }
                    self.validate_report_participants(report, &revision.episode, &source, &row, &report_input)?;
                    if let Some(intent) = &report_intent {
                        self.verify_investigation_intent(intent)?;
                        if let DiagnosticExecutionBinding::Shared { owner } = &intent.execution
                            && !revision.inputs.contains(owner)
                        {
                            return Err(integrity());
                        }
                    }
                }
            }
            let prior = current
                .checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.episodes.iter().find(|episode| episode.id == revision.episode.id));
            if revision.episode.lifecycle == DiagnosticLifecycle::Resolved
                && prior.is_none_or(|episode| episode.lifecycle != DiagnosticLifecycle::Resolved)
            {
                let proof = revision.episode.recovery.as_ref().ok_or_else(integrity)?;
                let matches_source = match &intent {
                    Some(intent) => {
                        proof.matches_source_with_intent(&revision.episode, &current.source, &current.parent, intent)
                    }
                    None => proof.matches_source(&revision.episode, &current.source, &current.parent),
                };
                if !matches_source {
                    return Err(integrity());
                }
                if matches!(proof, DiagnosticRecoveryProof::CompleteScope { .. })
                    && current.rows.iter().any(|row| {
                        matches!(&row.row.record, TelemetryRecord::Activity(observation)
                            if &observation.key == revision.episode.signal.activity_key())
                    })
                {
                    return Err(integrity());
                }
                if let DiagnosticRecoveryProof::FocusedParticipantGone { parent, .. } = proof {
                    let SourceObservation::FocusedPageWait { result, .. } = &current.source.observation else {
                        return Err(integrity());
                    };
                    self.verify_focus_parent(&result.focus.participant)?;
                    if !revision.inputs.contains(parent) {
                        return Err(integrity());
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_report_participants(
        &mut self,
        report: &DiagnosticReport,
        episode: &DiagnosticEpisode,
        source: &CollectionDescription,
        row: &ExportedRecord,
        input: &DiagnosticInput,
    ) -> Result<()> {
        match report {
            DiagnosticReport::Blocking { .. } => {
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Activity(observation) => Ok((&row.reference, observation.as_ref())),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_blocking_input(episode, source, rows) {
                    return Err(integrity());
                }
            }
            DiagnosticReport::CurrentRequestWork { .. } => {
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Operational(observation) => Ok((&row.reference, observation)),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_current_request_work_input(source, rows) {
                    return Err(integrity());
                }
            }
            DiagnosticReport::PageWaitWriters { blocking_contexts, .. } => {
                let SourceObservation::PageWaitWriters { focus, .. } = &source.observation else {
                    return Err(integrity());
                };
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Operational(observation) => Ok((&row.reference, observation)),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_writer_input(source, rows) {
                    return Err(integrity());
                }
                if *blocking_contexts != self.page_writer_contexts(focus)? {
                    return Err(integrity());
                }
            }
            DiagnosticReport::PageTaskHolders { .. } => {
                let SourceObservation::PageTaskHolders { focus, .. } = &source.observation else {
                    return Err(integrity());
                };
                self.verify_page_task_holder_focus(focus)?;
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Operational(observation) => Ok((&row.reference, observation)),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_task_holder_input(source, rows) {
                    return Err(integrity());
                }
            }
            DiagnosticReport::CompilationAdmission { .. } => {
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Operational(observation) => Ok((&row.reference, observation)),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_compiler_admission_input(source, rows) {
                    return Err(integrity());
                }
            }
            DiagnosticReport::RequestMemory { .. } => {
                let rows = input
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Operational(OperationalObservation {
                            fact: OperationalFact::ExecutionGrant(grant),
                            ..
                        }) => Ok((&row.reference, grant.as_ref())),
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !report.matches_request_memory_input(source, rows) {
                    return Err(integrity());
                }
            }
            DiagnosticReport::PageWait { blocking_contexts, .. } => {
                let SourceObservation::FocusedPageWait { result, .. } = &source.observation else {
                    return Err(integrity());
                };
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::PageWait(page), ..
                }) = &row.record
                else {
                    return Err(integrity());
                };
                let parent = self.diagnostic_input(
                    &result.focus.participant.parent.capture.observation_id,
                    input.controller_version,
                )?;
                let rows = parent
                    .rows
                    .iter()
                    .map(|row| match &row.row.record {
                        TelemetryRecord::Activity(observation) => {
                            Ok(ActivityFrameRow { observation, reference: Some(&row.reference) })
                        }
                        _ => Err(integrity()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let frame = ActivityFrameEvidence::new(&parent.source, &rows, true)?;
                let participation =
                    culprit_core::page_wait_blocking_participation(source, page, &result.focus.participant, &frame)?;
                if !blocking_contexts.iter().eq(participation.references()) {
                    return Err(integrity());
                }
            }
        }
        Ok(())
    }

    fn validate_investigation_event(&mut self, event: &DiagnosticInvestigationEvent) -> Result<()> {
        self.verify_investigation_intent(&event.intent)?;
        if let Some(attempt) = &event.attempt {
            let owner = DiagnosticInvestigationLineage::from_intent(&event.intent)?;
            if attempt.manifest_sha256.is_some() {
                if self.attempt_receipt(&attempt.capture, Some(&owner))? != *attempt {
                    return Err(integrity());
                }
            } else {
                // An allocation-only receipt preserves storage failure history without attesting damaged bytes.
                let capture = self.journal.capture(&attempt.capture.observation_id)?;
                if capture.identity != attempt.capture || !owner.matches_request(&capture.request) {
                    return Err(integrity());
                }
            }
        }
        if let DiagnosticInvestigationOutcome::Captured { capture } = &event.outcome {
            let input = self.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)?;
            if input.parent != *capture
                || !event.intent.matches_result_source(&input.source)
                || input
                    .investigation
                    .as_ref()
                    .is_none_or(|owner| !owner.matches_intent(&event.intent.focus, &event.intent))
            {
                return Err(integrity());
            }
        }
        Ok(())
    }
}

fn write_stream(files: &mut ObservationFiles, index: usize, records: &[TelemetryRecord]) -> Result<()> {
    let mut offset = 0;
    while offset < records.len() {
        files.begin_page(index).map_err(CollectionStop::error)?;
        let mut end = (offset + files.manifest.request.budget.page_rows as usize).min(records.len());
        loop {
            match files.write_page(index, &records[offset..end])? {
                None => break,
                Some(CollectionStop::ByteLimit | CollectionStop::RowLimit) if end - offset > 1 => {
                    end = offset + (end - offset) / 2;
                }
                Some(reason) => return Err(reason.error()),
            }
        }
        offset = end;
    }
    files.seal(index)
}
pub(in crate::outbox) fn checkpoint_parts(
    checkpoint: &DiagnosticCheckpoint,
    budget: &CollectionBudget,
) -> Result<Vec<DiagnosticCheckpointPart>> {
    let mut output = Vec::new();
    let mut part = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: checkpoint.controller_version,
        target: checkpoint.target.clone(),
        input: checkpoint.input.clone(),
        part: 0,
        parts: MAX_DIAGNOSTIC_EPISODES as u32,
        episodes: vec![],
    };
    // Reserve bounded envelope space; the actual ObservationFiles encoder still admits every final row.
    const ENVELOPE_ALLOWANCE_BYTES: usize = 2048;
    let limit = (budget.page_bytes as usize).checked_sub(ENVELOPE_ALLOWANCE_BYTES).ok_or_else(integrity)?;
    for episode in &checkpoint.episodes {
        part.episodes.push(episode.clone());
        if serde_json::to_vec(&part).map_err(|_| integrity())?.len() > limit {
            let last = part.episodes.pop().ok_or_else(integrity)?;
            if part.episodes.is_empty() {
                return Err(problem(OutboxProblem::Capacity));
            }
            output.push(part);
            part = DiagnosticCheckpointPart {
                id: String::new(),
                controller_version: checkpoint.controller_version,
                target: checkpoint.target.clone(),
                input: checkpoint.input.clone(),
                part: output.len() as u32,
                parts: MAX_DIAGNOSTIC_EPISODES as u32,
                episodes: vec![last],
            };
            if serde_json::to_vec(&part).map_err(|_| integrity())?.len() > limit {
                return Err(problem(OutboxProblem::Capacity));
            }
        }
    }
    output.push(part);
    let count = output.len() as u32;
    for part in &mut output {
        part.parts = count;
        part.id = part.expected_id()?;
    }
    Ok(output)
}

fn validate_new_episode(
    input: &DiagnosticInput,
    episode: &DiagnosticEpisode,
    original: Option<&ActivityFrameEvidence<'_>>,
) -> Result<()> {
    let row = input.rows.iter().find(|row| row.reference == episode.first).ok_or_else(integrity)?;
    if matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }) {
        let frame = original.ok_or_else(integrity)?;
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
            &row.row.record
        else {
            return Err(integrity());
        };
        let DiagnosticFocusPlan::Ready(focus) =
            plan_page_task_holder_focus(&input.source, page, &row.reference, frame)?
        else {
            return Err(integrity());
        };
        if !episode.matches_focus(&focus) || episode.scope != DiagnosticScope::from_source(frame.source())? {
            return Err(integrity());
        }
    } else if episode.scope != DiagnosticScope::from_source(&input.source)? {
        return Err(integrity());
    }
    Ok(())
}
