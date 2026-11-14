use super::*;
use crate::collection_files::ObservationFiles;

fn local_budget() -> CollectionBudget {
    CollectionBudget {
        page_rows: 128,
        max_snapshot_rows: (MAX_DIAGNOSTIC_EPISODES * 2) as u32,
        page_bytes: 256 * 1024,
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
        let existing: Option<(String, String, Option<String>)> = self
            .journal
            .conn
            .query_row(
                "SELECT observation_id,update_json,result_json FROM diagnostic_steps WHERE input_id=?1 AND step_id=?2",
                params![input.parent.capture.observation_id, update.step_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(storage)?;
        if let Some((id, json, result)) = existing {
            let previous: DiagnosticUpdate = decode(&json)?;
            if !same_update(&previous, &update) {
                return Err(DeliveryProblem::Conflict.error());
            }
            if let Some(result) = result {
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
        let budget = local_budget();
        let parts = checkpoint_parts(&update.checkpoint, &budget)?;
        let request = CollectionRequest {
            subject: match &input.source.target {
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
            target: input.source.target.clone(),
            source: DIAGNOSTICS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
                parent: input.parent.clone(),
                controller_version: input.controller_version,
                step_id: update.step_id.clone(),
                analyzed_at: update.analyzed_at.clone(),
                records: update.revisions.len() as u32,
                checkpoint_records: parts.len() as u32,
            }),
        };
        let key = update.checkpoint.key()?;
        self.journal
            .conn
            .execute(
                "INSERT INTO diagnostic_steps VALUES(?1,?2,?3,?4,?5,?6,NULL)",
                params![
                    identity.observation_id,
                    input.parent.capture.observation_id,
                    key,
                    input.expected_head,
                    update.step_id,
                    encode(&update)?
                ],
            )
            .map_err(storage)?;
        let mut files = ObservationFiles::create(&reservation.directory, request, identity.clone())?;
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
        {
            return Err(DeliveryProblem::Conflict.error());
        }
        if !valid_digest(&update.step_id)
            || canonical_time(&update.analyzed_at).is_err()
            || !update.checkpoint.is_valid()
            || update.checkpoint.input != input.parent
            || update.checkpoint.target != input.source.target
            || update.checkpoint.controller_version != input.controller_version
            || update.revisions.len() > MAX_DIAGNOSTIC_EPISODES
            || encode(update)?.len() > 2 * MAX_CHECKPOINT_BYTES
        {
            return Err(integrity());
        }
        super::journal::validate_transition(&self.journal.conn, &update.checkpoint.key()?, update)?;
        let mut known: BTreeSet<String> = current.rows.iter().map(|row| row.reference.key()).collect::<Result<_>>()?;
        if let Some(previous) = &current.checkpoint {
            for reference in super::journal::checkpoint_references(previous) {
                known.insert(reference.key()?);
            }
        }
        for revision in &update.revisions {
            if revision.previous_revision.is_none()
                && (revision.episode.scope != DiagnosticScope::from_source(&current.source)?
                    || !current.rows.iter().any(|row| row.reference == revision.episode.first))
            {
                return Err(integrity());
            }
            if !revision.is_valid()
                || revision.controller_version != input.controller_version
                || revision.analyzed_at != update.analyzed_at
                || revision.episode.scope.target.telemetry_id()? != input.source.target.telemetry_id()?
            {
                return Err(integrity());
            }
            for reference in &revision.inputs {
                if !known.contains(&reference.key()?) {
                    let (source, _) = self.diagnostic_reference(reference)?;
                    if source.target.telemetry_id()? != revision.episode.scope.target.telemetry_id()? {
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
                    let SourceObservation::FocusedPageWait { result, .. } = &source.observation else {
                        return Err(integrity());
                    };
                    let TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::PageWait(page),
                        ..
                    }) = &row.record
                    else {
                        return Err(integrity());
                    };
                    if !revision.episode.matches_focus(&result.focus) {
                        return Err(integrity());
                    }
                    self.verify_focus_parent(&result.focus)?;
                    if source.target.telemetry_id()? != revision.episode.scope.target.telemetry_id()?
                        || !revision.inputs.contains(&result.focus.parent)
                        || !result.accepts_observation(page)
                    {
                        return Err(integrity());
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
                if !proof.matches_source(&revision.episode, &current.source, &current.parent) {
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
                    self.verify_focus_parent(&result.focus)?;
                    if !revision.inputs.contains(parent) {
                        return Err(integrity());
                    }
                }
            }
        }
        Ok(())
    }
}

fn same_update(left: &DiagnosticUpdate, right: &DiagnosticUpdate) -> bool {
    right.checkpoint.is_valid()
        && right.revisions.iter().all(DiagnosticRevision::is_valid)
        && left.step_id == right.step_id
        && left.checkpoint == right.checkpoint
        && left.revisions.iter().map(|row| &row.id).eq(right.revisions.iter().map(|row| &row.id))
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
