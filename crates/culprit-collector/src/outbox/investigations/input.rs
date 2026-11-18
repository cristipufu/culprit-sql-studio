use super::*;
mod rows;
use rows::{RowSelection, referenced_rows};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputInstallation {
    Pending,
    EvidenceOnly,
}

struct StoredDiagnosticInput {
    parent: String,
    source: String,
    connection: String,
    restored: Option<String>,
    checksum: Option<String>,
    evidence_only: bool,
    investigation: Option<String>,
    charged_bytes: i64,
}

struct StoredDiagnosticAcknowledgement {
    parent: String,
    source: String,
    controller_version: u32,
    acknowledged: bool,
    evidence_only: bool,
    investigation: Option<String>,
}

struct DiagnosticInputContents {
    parent: String,
    source: String,
    connection: String,
    version: u32,
    restored: Option<String>,
    checksum: Option<String>,
    investigation: Option<String>,
}

impl Files {
    pub(in crate::outbox) fn diagnostic_input_state(
        &self,
        reference: &DiagnosticCaptureRef,
        controller_version: u32,
    ) -> Result<DiagnosticInputState> {
        if !reference.is_valid() || controller_version == 0 {
            return Err(integrity());
        }
        let stored = self.journal.conn.query_row(
            "SELECT parent_json,source_json,controller_version,acknowledged,evidence_only,lineage_json FROM diagnostic_inputs WHERE observation_id=?1",
            [&reference.capture.observation_id],
            |row| Ok(StoredDiagnosticAcknowledgement {
                parent: row.get(0)?, source: row.get(1)?, controller_version: row.get(2)?,
                acknowledged: row.get(3)?, evidence_only: row.get(4)?,
                investigation: row.get(5)?,
            }),
        ).optional().map_err(storage)?;
        let Some(stored) = stored else { return Ok(DiagnosticInputState::Missing) };
        let parent: DiagnosticCaptureRef = decode(&stored.parent)?;
        let source: CollectionDescription = decode(&stored.source)?;
        let investigation = decode_investigation(stored.investigation.as_deref())?;
        if parent != *reference
            || stored.controller_version != controller_version
            || stored.evidence_only && !stored.acknowledged
            || source.captured_range().map_err(|_| integrity())?.scope_id != parent.source_scope_id
            || !eligible_source(&source, investigation.as_ref())? && !stored.evidence_only
        {
            return Err(integrity());
        }
        Ok(if stored.evidence_only {
            DiagnosticInputState::EvidenceOnly
        } else if stored.acknowledged {
            DiagnosticInputState::Consumed
        } else {
            DiagnosticInputState::Pending
        })
    }

    pub(in crate::outbox) fn next_diagnostic_input(
        &mut self,
        controller_version: u32,
    ) -> Result<Option<DiagnosticInput>> {
        let id: Option<String> = self.journal.conn.query_row(
            "SELECT observation_id FROM diagnostic_inputs WHERE acknowledged=0 AND controller_version=?1 ORDER BY rowid LIMIT 1",
            [controller_version], |row| row.get(0)).optional().map_err(storage)?;
        id.map(|id| self.diagnostic_input(&id, controller_version)).transpose()
    }
    pub(in crate::outbox) fn diagnostic_input(&mut self, id: &str, controller_version: u32) -> Result<DiagnosticInput> {
        let stored = self.journal.conn.query_row(
            "SELECT parent_json,source_json,connection,controller_version,restored_json,restored_sha,lineage_json FROM diagnostic_inputs WHERE observation_id=?1",
            [id], |row| Ok(DiagnosticInputContents {
                parent: row.get(0)?, source: row.get(1)?, connection: row.get(2)?, version: row.get(3)?,
                restored: row.get(4)?, checksum: row.get(5)?, investigation: row.get(6)?,
            })).optional().map_err(storage)?
            .ok_or_else(|| DeliveryProblem::Conflict.error())?;
        let parent: DiagnosticCaptureRef = decode(&stored.parent)?;
        let source: CollectionDescription = decode(&stored.source)?;
        let investigation = decode_investigation(stored.investigation.as_deref())?;
        if !parent.is_valid()
            || parent.capture.observation_id != id
            || stored.version != controller_version
            || source.captured_range()?.scope_id != parent.source_scope_id
        {
            return Err(integrity());
        }
        let rows = if let Some(restored) = stored.restored {
            if restored.len() > MAX_RESTORED_INPUT_BYTES
                || stored.checksum.as_deref()
                    != Some(restored_checksum(&parent, &source, investigation.as_ref(), &restored)?.as_str())
            {
                return Err(integrity());
            }
            row_references(&parent, &source, decode(&restored)?)?
        } else {
            let (validated, rows) = self.validated_rows(id, RowSelection::Diagnostic)?;
            if validated.manifest_sha != parent.manifest_sha256
                || validated.manifest.description.as_ref() != Some(&source)
                || encode_investigation(validated.manifest.investigation.as_ref())?
                    != encode_investigation(investigation.as_ref())?
            {
                return Err(integrity());
            }
            referenced_rows(&parent, &source, rows)?
        };
        let key = controller_key(&parent, controller_target(&source, investigation.as_ref())?, controller_version)?;
        let head: Option<(String, String)> = self
            .journal
            .conn
            .query_row(
                "SELECT observation_id,checkpoint_json FROM diagnostic_heads WHERE controller_key=?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let (expected_head, checkpoint) = match head {
            Some((capture, json)) => {
                let checkpoint: DiagnosticCheckpoint = decode(&json)?;
                if !checkpoint.is_valid() || checkpoint.key()? != key {
                    return Err(integrity());
                }
                (Some(capture), Some(checkpoint))
            }
            None => (None, None),
        };
        Ok(DiagnosticInput {
            parent: parent.clone(),
            source: source.clone(),
            investigation,
            connection: stored.connection,
            rows,
            controller_version,
            checkpoint,
            expected_head,
        })
    }

    pub(in crate::outbox) fn diagnostic_reference(
        &mut self,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<(CollectionDescription, ExportedRecord)> {
        if !reference.is_valid() {
            return Err(integrity());
        }
        let id = &reference.capture.observation_id;
        let restored: Option<DiagnosticInputContents> = self
            .journal
            .conn
            .query_row(
                "SELECT parent_json,source_json,connection,controller_version,restored_json,restored_sha,lineage_json FROM diagnostic_inputs WHERE observation_id=?1",
                [id],
                |row| Ok(DiagnosticInputContents {
                    parent: row.get(0)?, source: row.get(1)?, connection: row.get(2)?, version: row.get(3)?,
                    restored: row.get(4)?, checksum: row.get(5)?, investigation: row.get(6)?,
                }),
            )
            .optional()
            .map_err(storage)?;
        let (parent, source, rows) = if let Some(stored) = restored.filter(|stored| stored.restored.is_some()) {
            let restored = stored.restored.as_deref().ok_or_else(integrity)?;
            let parent = decode::<DiagnosticCaptureRef>(&stored.parent)?;
            let source = decode::<CollectionDescription>(&stored.source)?;
            let investigation = decode_investigation(stored.investigation.as_deref())?;
            if restored.len() > MAX_RESTORED_INPUT_BYTES
                || stored.checksum.as_deref()
                    != Some(restored_checksum(&parent, &source, investigation.as_ref(), restored)?.as_str())
            {
                return Err(integrity());
            }
            let rows = row_references(&parent, &source, decode::<Vec<ExportedRecord>>(restored)?)?;
            (parent, source, rows)
        } else {
            let (validated, rows) = self.validated_rows(id, RowSelection::All)?;
            let parent = DiagnosticCaptureRef {
                capture: validated.manifest.capture.clone(),
                manifest_sha256: validated.manifest_sha.clone(),
                source_scope_id: validated.range()?.scope_id,
            };
            let source = validated.manifest.description.ok_or_else(integrity)?;
            let rows = referenced_rows(&parent, &source, rows)?;
            (parent, source, rows)
        };
        if parent != reference.capture_ref() {
            return Err(integrity());
        }
        rows.into_iter().find(|row| row.reference == *reference).map(|row| (source, row.row)).ok_or_else(integrity)
    }

    pub(in crate::outbox) fn verify_focus_parent(&mut self, focus: &ActivityRequestFocus) -> Result<()> {
        let (source, row) = self.diagnostic_reference(&focus.parent)?;
        let TelemetryRecord::Activity(original) = &row.record else {
            return Err(integrity());
        };
        if !focus.matches_parent(&source, original, &focus.parent) {
            return Err(integrity());
        }
        Ok(())
    }

    pub(in crate::outbox) fn verify_page_writer_focus(&mut self, focus: &PageWaitWriterFocus) -> Result<()> {
        self.page_writer_contexts(focus).map(|_| ())
    }

    /// The original frame is authenticated before native page capture. Its member/role are never filled later.
    pub(in crate::outbox) fn verified_page_task_activity(
        &mut self,
        activity: &DiagnosticCaptureRef,
    ) -> Result<(DiagnosticInput, NativeReadContext, Vec<ActivityRequestSelector>)> {
        if !activity.is_valid() {
            return Err(integrity());
        }
        let original = self.diagnostic_input(&activity.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)?;
        if original.parent != *activity || !matches!(original.source.observation, SourceObservation::Activity(_)) {
            return Err(integrity());
        }
        let frame = original.activity_frame()?;
        let context = frame.context();
        if !context.has_qualified_local_owner()
            || frame.native().database_writable.is_none()
            || frame.native().scope != culprit_core::MeasurementScope::Database
            || original.source.target.database().is_none()
            || original.source.target.engine() != culprit_core::Engine::Mssql
            || original.source.source != culprit_core::source_id::SESSIONS
        {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::message!("collection.unsupported"),
            ));
        }
        let requests = frame.page_wait_task_requests()?;
        Ok((original, context, requests))
    }

    pub(in crate::outbox) fn verify_page_task_holder_focus(&mut self, focus: &PageTaskHolderFocus) -> Result<()> {
        if !focus.is_valid() {
            return Err(integrity());
        }
        let parent =
            self.diagnostic_input(&focus.participant.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)?;
        let frame = parent.activity_frame()?;
        if !focus.matches_activity_frame(&frame) {
            return Err(integrity());
        }
        let (source, row) = self.diagnostic_reference(&focus.page)?;
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
            &row.record
        else {
            return Err(integrity());
        };
        if !focus.matches_page(&source, page, &focus.page) {
            return Err(integrity());
        }
        Ok(())
    }

    pub(super) fn page_writer_contexts(&mut self, focus: &PageWaitWriterFocus) -> Result<Vec<DiagnosticEvidenceRef>> {
        if !focus.is_valid() {
            return Err(integrity());
        }
        let original = &focus.waiter.participant;
        let parent = self.diagnostic_input(&original.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)?;
        let frame = parent.activity_frame()?;
        if !focus.matches_activity_frame(&frame) {
            return Err(integrity());
        }
        let (source, row) = self.diagnostic_reference(&focus.page)?;
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
            &row.record
        else {
            return Err(integrity());
        };
        if !focus.matches_page(&source, page, &focus.page) {
            return Err(integrity());
        }
        let participation = culprit_core::page_wait_blocking_participation(&source, page, original, &frame)?;
        Ok(participation.references().cloned().collect())
    }
    pub(in crate::outbox) fn verify_source_read_parent(
        &mut self,
        focus: &SourceReadFocus,
        execution: &NativeSourceReadScope,
    ) -> Result<()> {
        let (source, row) = self.diagnostic_reference(&focus.basis)?;
        if !focus.matches_basis(&source, &row, execution) {
            return Err(integrity());
        }
        let Some(prerequisite) = focus.prerequisite.compilation_admission() else { return Ok(()) };
        let context =
            self.diagnostic_input(&prerequisite.observation.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)?;
        let SourceObservation::FocusedCompilationAdmission { result, .. } = &context.source.observation else {
            return Err(integrity());
        };
        self.verify_focus_parent(&result.focus)?;
        let rows = context
            .rows
            .iter()
            .map(|row| match &row.row.record {
                TelemetryRecord::Operational(native) => Ok((&row.reference, native)),
                _ => Err(integrity()),
            })
            .collect::<Result<Vec<_>>>()?;
        if context.parent != prerequisite.observation.capture_ref()
            || !focus.matches_prerequisite_input(&source, &row, &context.source, rows)
        {
            return Err(integrity());
        }
        Ok(())
    }
    pub(in crate::outbox) fn verify_investigation_intent(
        &mut self,
        intent: &DiagnosticInvestigationIntent,
    ) -> Result<()> {
        if !intent.is_valid() {
            return Err(integrity());
        }
        match &intent.focus {
            DiagnosticFocus::PageWaitWriters { focus } => self.verify_page_writer_focus(focus)?,
            DiagnosticFocus::PageTaskHolders { focus } => self.verify_page_task_holder_focus(focus)?,
            DiagnosticFocus::PageWaitTasks { focus, original_context, original_requests } => {
                let (original, context, requests) = self.verified_page_task_activity(&focus.parent.capture_ref())?;
                if context != **original_context
                    || requests != *original_requests
                    || !original.rows.iter().any(|row| {
                        matches!(&row.row.record, TelemetryRecord::Activity(observation)
                        if intent.focus.matches_parent(&original.source, observation, &row.reference))
                    })
                {
                    return Err(integrity());
                }
            }
            DiagnosticFocus::SourceRead { focus } => {
                let DiagnosticExecutionBinding::NativeRead { execution } = &intent.execution else {
                    return Err(integrity());
                };
                self.verify_source_read_parent(focus, execution)?;
            }
            _ => self.verify_focus_parent(intent.focus.activity_participant().ok_or_else(integrity)?)?,
        }
        if let DiagnosticExecutionBinding::Shared { owner } = &intent.execution {
            let (source, row) = self.diagnostic_reference(owner)?;
            if !intent.matches_shared_owner(&source, &row) {
                return Err(integrity());
            }
        }
        Ok(())
    }

    pub(in crate::outbox) fn diagnostic_owner(
        &mut self,
        identity: &CaptureIdentity,
    ) -> Result<Option<(DiagnosticEvidenceRef, CollectionDescription, ExportedRecord)>> {
        let (validated, rows) = self.validated_rows(&identity.observation_id, RowSelection::SharedOwner)?;
        if validated.manifest.capture != *identity {
            return Err(integrity());
        }
        let source = validated.manifest.description.clone().ok_or_else(integrity)?;
        if !matches!(source.target, CollectionTarget::Shared(_)) {
            return Ok(None);
        }
        if !shared_owner_source(&source) {
            return Ok(None);
        }
        let parent = DiagnosticCaptureRef {
            capture: identity.clone(),
            manifest_sha256: validated.manifest_sha.clone(),
            source_scope_id: source.captured_range()?.scope_id,
        };
        let rows = referenced_rows(&parent, &source, rows)?;
        Ok(rows.into_iter().find_map(|row| {
            let TelemetryRecord::Operational(observation) = &row.row.record else { return None };
            let positive = source.observation.snapshot()?.sources.iter().any(|coverage| {
                coverage.source == observation.source
                    && coverage.records > 0
                    && matches!(
                        coverage.state,
                        culprit_core::SourceState::Collected | culprit_core::SourceState::Partial
                    )
            });
            positive.then(|| (row.reference, source.clone(), row.row))
        }))
    }
    pub(in crate::outbox) fn restore_diagnostic_input(&mut self, input: RestoredInput) -> Result<()> {
        self.install_diagnostic_input(input, InputInstallation::Pending)
    }
    pub(in crate::outbox) fn restore_diagnostic_input_at_generation(
        &mut self,
        input: RestoredInput,
        generation: i64,
    ) -> Result<()> {
        if super::retirement::generation(&self.journal.conn)? != generation {
            return Err(DeliveryProblem::Conflict.error());
        }
        self.restore_diagnostic_input(input)
    }
    pub(in crate::outbox) fn cache_diagnostic_evidence(&mut self, input: RestoredInput) -> Result<()> {
        self.install_diagnostic_input(input, InputInstallation::EvidenceOnly)
    }
    fn install_diagnostic_input(&mut self, input: RestoredInput, installation: InputInstallation) -> Result<()> {
        if !matches!(&self.authority,super::super::RevisionAuthority::Recovery {store_id} if *store_id==input.store_id)
        {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if !input.parent.is_valid()
            || input.connection.is_empty()
            || input.connection.len() > 256
            || input.connection.chars().any(char::is_control)
        {
            return Err(DeliveryProblem::Conflict.error());
        }
        let investigation = encode_investigation(input.investigation.as_ref())?;
        let accepted = match installation {
            InputInstallation::Pending => eligible_source(&input.source, input.investigation.as_ref())?,
            InputInstallation::EvidenceOnly => evidence_cache_source(&input.source, input.investigation.as_ref())?,
        };
        if !accepted {
            return Err(integrity());
        }
        let rows = row_references(&input.parent, &input.source, input.rows.clone())?;
        if rows.iter().any(|row| !row.row.record.is_valid()) {
            return Err(integrity());
        }
        if matches!(input.source.observation, SourceObservation::SourceRead { .. }) && !input.rows.is_empty() {
            return Err(integrity());
        }
        let encoded = encode(&input.rows)?;
        let checksum = restored_checksum(&input.parent, &input.source, input.investigation.as_ref(), &encoded)?;
        if encoded.len() > MAX_RESTORED_INPUT_BYTES {
            return Err(problem(OutboxProblem::Capacity));
        }
        let existing: Option<StoredDiagnosticInput> = self
            .journal
            .conn
            .query_row(
                "SELECT parent_json,source_json,restored_json,restored_sha,evidence_only,lineage_json,charged_bytes,connection FROM diagnostic_inputs WHERE observation_id=?1",
                [&input.parent.capture.observation_id],
                |row| Ok(StoredDiagnosticInput {
                    parent: row.get(0)?,
                    source: row.get(1)?,
                    restored: row.get(2)?,
                    checksum: row.get(3)?,
                    evidence_only: row.get(4)?,
                    investigation: row.get(5)?,
                    charged_bytes: row.get(6)?,
                    connection: row.get(7)?,
                }),
            )
            .optional()
            .map_err(storage)?;
        if let Some(stored) = &existing {
            if decode::<DiagnosticCaptureRef>(&stored.parent)? != input.parent
                || decode::<CollectionDescription>(&stored.source)? != input.source
                || stored.investigation != investigation
            {
                return Err(integrity());
            }
            if let Some(existing_rows) = &stored.restored {
                if existing_rows != &encoded || stored.checksum.as_deref() != Some(checksum.as_str()) {
                    return Err(integrity());
                }
                if installation == InputInstallation::Pending && stored.evidence_only {
                    let tx = self.journal.conn.transaction().map_err(storage)?;
                    tx.execute(
                        "UPDATE diagnostic_inputs SET acknowledged=0,evidence_only=0 WHERE observation_id=?1",
                        [&input.parent.capture.observation_id],
                    )
                    .map_err(storage)?;
                    tx.execute(
                        "INSERT OR IGNORE INTO diagnostic_pins VALUES(?1,?2)",
                        params![
                            format!("input:{}", input.parent.capture.observation_id),
                            input.parent.capture.observation_id
                        ],
                    )
                    .map_err(storage)?;
                    pin_restored_focus(&tx, &input)?;
                    tx.commit().map_err(storage)?;
                }
                return Ok(());
            }
        } else if self.epoch.is_some() && installation == InputInstallation::Pending {
            return Err(DeliveryProblem::Conflict.error());
        }
        let parent_json = encode(&input.parent)?;
        let source_json = encode(&input.source)?;
        let metadata_charge = match &existing {
            Some(stored) => super::journal::input_metadata_charge(
                &stored.parent,
                &stored.source,
                &stored.connection,
                stored.investigation.as_deref(),
            )?,
            None => super::journal::input_metadata_charge(
                &parent_json,
                &source_json,
                &input.connection,
                investigation.as_deref(),
            )?,
        };
        let charge =
            metadata_charge.checked_add(encoded.len() as u64).ok_or_else(|| problem(OutboxProblem::Capacity))?;
        let previous_charge = existing
            .as_ref()
            .map(|stored| super::super::journal::bytes_from_sql(stored.charged_bytes))
            .transpose()?
            .unwrap_or(0);
        let additional_charge = charge.checked_sub(previous_charge).ok_or_else(integrity)?;
        let status = self.journal.status(1, None)?;
        if status.charged_bytes.checked_add(additional_charge).is_none_or(|total| total > self.journal.limits.max_bytes)
        {
            return Err(problem(OutboxProblem::Capacity));
        }
        let tx = self.journal.conn.transaction().map_err(storage)?;
        if existing.is_some() {
            tx.execute(
                "UPDATE diagnostic_inputs SET restored_json=?1,charged_bytes=?2,restored_sha=?4 WHERE observation_id=?3",
                params![encoded, super::super::journal::bytes_to_sql(charge)?, input.parent.capture.observation_id,checksum],
            )
            .map_err(storage)?;
        } else {
            tx.execute(
                "INSERT INTO diagnostic_inputs VALUES(?1,?2,?3,?4,?5,?9,?6,?7,?8,?9,?10)",
                params![
                    input.parent.capture.observation_id,
                    DIAGNOSTIC_CONTROLLER_VERSION,
                    parent_json,
                    source_json,
                    input.connection,
                    encoded,
                    super::super::journal::bytes_to_sql(charge)?,
                    checksum,
                    installation == InputInstallation::EvidenceOnly,
                    investigation
                ],
            )
            .map_err(storage)?;
        }
        if installation == InputInstallation::Pending {
            tx.execute("UPDATE diagnostic_inputs SET acknowledged=0,evidence_only=0 WHERE observation_id=?1 AND evidence_only=1",[&input.parent.capture.observation_id]).map_err(storage)?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO diagnostic_pins SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM diagnostic_inputs WHERE observation_id=?2 AND acknowledged=0)",
            params![format!("input:{}", input.parent.capture.observation_id), input.parent.capture.observation_id],
        )
        .map_err(storage)?;
        if installation == InputInstallation::Pending {
            pin_restored_focus(&tx, &input)?;
        }
        tx.commit().map_err(storage)
    }
}

fn pin_restored_focus(tx: &Transaction<'_>, input: &RestoredInput) -> Result<()> {
    if let SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } = &input.source.observation {
        tx.execute(
            "INSERT OR IGNORE INTO diagnostic_pins SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM diagnostic_inputs WHERE observation_id=?3 AND acknowledged=0)",
            params![format!("focus:{}",input.parent.capture.observation_id),activity.capture.observation_id,input.parent.capture.observation_id],
        ).map_err(storage)?;
    }
    let saved_focus = investigation_intent(&input.source, input.investigation.as_ref())?.map(|intent| intent.focus);
    if let Some(focus) = saved_focus.or_else(|| input.source.observation.diagnostic_focus()) {
        let captures = focus.references().map(|reference| &reference.capture.observation_id).collect::<BTreeSet<_>>();
        for capture in captures {
            tx.execute(
                "INSERT OR IGNORE INTO diagnostic_pins SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM diagnostic_inputs WHERE observation_id=?3 AND acknowledged=0)",
                params![format!("focus:{}",input.parent.capture.observation_id),capture,input.parent.capture.observation_id],
            ).map_err(storage)?;
        }
        if let Some(intent) = investigation_intent(&input.source, input.investigation.as_ref())?
            && let DiagnosticExecutionBinding::Shared { owner } = &intent.execution
        {
            tx.execute(
                "INSERT OR IGNORE INTO diagnostic_pins SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM diagnostic_inputs WHERE observation_id=?3 AND acknowledged=0)",
                params![format!("focus:{}",input.parent.capture.observation_id),owner.capture.observation_id,input.parent.capture.observation_id],
            ).map_err(storage)?;
        }
    }
    Ok(())
}

impl Files {
    pub(in crate::outbox) fn restore_diagnostic_checkpoint(
        &mut self,
        recovered: crate::delivery::diagnostic_recovery::RecoveredDiagnosticCheckpoint,
    ) -> Result<()> {
        if self.epoch.is_some() {
            return Err(DeliveryProblem::Conflict.error());
        }
        let (range, checkpoint) = recovered.into_parts();
        if !matches!(&self.authority,super::super::RevisionAuthority::Recovery {store_id} if *store_id==range.store_id())
        {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let reference = crate::delivery::diagnostic_recovery::capture_reference(range.manifest())?;
        if !checkpoint.is_valid()
            || checkpoint.target != range.manifest().source.target
            || encode(&checkpoint)?.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(integrity());
        }
        let key = checkpoint.key()?;
        let existing: Option<(String, String)> = self
            .journal
            .conn
            .query_row(
                "SELECT capture_json,checkpoint_json FROM diagnostic_heads WHERE controller_key=?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        if let Some((receipt, json)) = existing {
            let before: DiagnosticCaptureRef = decode(&receipt)?;
            let previous: DiagnosticCheckpoint = decode(&json)?;
            if !before.is_valid() || !previous.is_valid() || previous.key()? != key {
                return Err(integrity());
            }
            let prior = before.capture.revision.as_ref().ok_or_else(integrity)?;
            let current = reference.capture.revision.as_ref().ok_or_else(integrity)?;
            if prior.outbox_id != current.outbox_id {
                return Err(DeliveryProblem::Conflict.error());
            }
            let rank = |revision: &CaptureRevision| -> Result<(u64, u64)> {
                Ok((
                    revision.owner_epoch.parse().map_err(|_| integrity())?,
                    revision.sequence.parse().map_err(|_| integrity())?,
                ))
            };
            match rank(current)?.cmp(&rank(prior)?) {
                std::cmp::Ordering::Less => return Ok(()),
                std::cmp::Ordering::Equal => {
                    if before != reference || previous != checkpoint {
                        return Err(integrity());
                    }
                    return Ok(());
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        let tx = self.journal.conn.transaction().map_err(storage)?;
        tx.execute(
            "INSERT INTO diagnostic_heads VALUES(?1,?2,?3,?4,?5) ON CONFLICT(controller_key) DO UPDATE SET controller_version=excluded.controller_version,observation_id=excluded.observation_id,checkpoint_json=excluded.checkpoint_json,capture_json=excluded.capture_json",
            params![key,checkpoint.controller_version, reference.capture.observation_id, encode(&checkpoint)?,encode(&reference)?],
        )
        .map_err(storage)?;
        tx.execute("DELETE FROM diagnostic_episodes WHERE controller_key=?1", [&key]).map_err(storage)?;
        for episode in &checkpoint.episodes {
            tx.execute(
                "INSERT INTO diagnostic_episodes VALUES(?1,?2,?3,?4)",
                params![key, episode.id, episode.revision, encode(episode)?],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }
}

impl Files {
    pub(in crate::outbox) fn diagnostic_checkpoints(
        &self,
        version: u32,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<DiagnosticCheckpointPage> {
        if version == 0 || !(1..=128).contains(&limit) {
            return Err(problem(OutboxProblem::InvalidLimits));
        }
        let after = match cursor {
            Some(value) if valid_digest(value) => value,
            None => "",
            _ => return Err(integrity()),
        };
        let mut query=self.journal.conn.prepare("SELECT controller_key,checkpoint_json FROM diagnostic_heads WHERE controller_key>?1 AND controller_version=?3 ORDER BY controller_key LIMIT ?2").map_err(storage)?;
        let mut rows = query.query(params![after, limit + 1, version]).map_err(storage)?;
        let mut checkpoints = Vec::new();
        let mut last = None;
        let mut more = false;
        while let Some(row) = rows.next().map_err(storage)? {
            let key: String = row.get(0).map_err(storage)?;
            let json: String = row.get(1).map_err(storage)?;
            let checkpoint: DiagnosticCheckpoint = decode(&json)?;
            if json.len() > MAX_CHECKPOINT_BYTES || !checkpoint.is_valid() || checkpoint.key()? != key {
                return Err(integrity());
            }
            if checkpoints.len() == limit as usize {
                more = true;
                break;
            }
            last = Some(key);
            if checkpoint.controller_version == version {
                checkpoints.push(checkpoint);
            }
        }
        Ok(DiagnosticCheckpointPage { checkpoints, next_cursor: if more { last } else { None } })
    }
}
