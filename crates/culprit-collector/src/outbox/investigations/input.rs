use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputInstallation {
    Pending,
    EvidenceOnly,
}

struct StoredDiagnosticInput {
    parent: String,
    source: String,
    restored: Option<String>,
    checksum: Option<String>,
    evidence_only: bool,
}

impl Files {
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
        let (parent, source, connection, version, restored, restored_sha): (String,String,String,u32,Option<String>,Option<String>) = self.journal.conn.query_row(
            "SELECT parent_json,source_json,connection,controller_version,restored_json,restored_sha FROM diagnostic_inputs WHERE observation_id=?1",
            [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).map_err(storage)?;
        let parent: DiagnosticCaptureRef = decode(&parent)?;
        let source: CollectionDescription = decode(&source)?;
        if !parent.is_valid()
            || parent.capture.observation_id != id
            || version != controller_version
            || source.captured_range()?.scope_id != parent.source_scope_id
        {
            return Err(integrity());
        }
        let rows = if let Some(restored) = restored {
            if restored.len() > MAX_RESTORED_INPUT_BYTES
                || restored_sha.as_deref() != Some(digest(restored.as_bytes()).as_str())
            {
                return Err(integrity());
            }
            decode(&restored)?
        } else {
            let validated = self.validated(id)?;
            if validated.manifest_sha != parent.manifest_sha256
                || validated.manifest.description.as_ref() != Some(&source)
            {
                return Err(integrity());
            }
            self.exported_rows(&validated)?
        };
        let key = controller_key(&parent, &source.target, controller_version)?;
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
            connection,
            rows: row_references(&parent, &source, rows)?,
            controller_version,
            checkpoint,
            expected_head,
        })
    }

    fn exported_rows(&self, validated: &ValidatedCapture) -> Result<Vec<ExportedRecord>> {
        let mut output = Vec::new();
        for file in &validated.manifest.files {
            let input = crate::durable_fs::regular_file(
                &self.directory(&validated.manifest.capture.observation_id).join(&file.file),
            )
            .map_err(storage)?;
            let mut input = BufReader::new(input.take(u64::from(file.bytes) + 1));
            let mut line = Vec::new();
            loop {
                line.clear();
                if (&mut input)
                    .take(u64::from(validated.manifest.request.budget.page_bytes) + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(storage)?
                    == 0
                {
                    break;
                }
                if line.len() > validated.manifest.request.budget.page_bytes as usize || line.last() != Some(&b'\n') {
                    return Err(integrity());
                }
                output.push(serde_json::from_slice(&line).map_err(|_| integrity())?);
                if output.len() > 100_000 {
                    return Err(integrity());
                }
            }
        }
        Ok(output)
    }

    pub(in crate::outbox) fn diagnostic_reference(
        &mut self,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<(CollectionDescription, ExportedRecord)> {
        if !reference.is_valid() {
            return Err(integrity());
        }
        let id = &reference.capture.observation_id;
        let restored: Option<(Option<String>, Option<String>)> = self
            .journal
            .conn
            .query_row(
                "SELECT restored_json,restored_sha FROM diagnostic_inputs WHERE observation_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let (parent, source, rows) = if let Some((Some(restored), restored_sha)) = restored {
            if restored.len() > MAX_RESTORED_INPUT_BYTES
                || restored_sha.as_deref() != Some(digest(restored.as_bytes()).as_str())
            {
                return Err(integrity());
            }
            let (parent, source): (String, String) = self
                .journal
                .conn
                .query_row(
                    "SELECT parent_json,source_json FROM diagnostic_inputs WHERE observation_id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(storage)?;
            (
                decode::<DiagnosticCaptureRef>(&parent)?,
                decode::<CollectionDescription>(&source)?,
                decode::<Vec<ExportedRecord>>(&restored)?,
            )
        } else {
            let validated = self.validated(id)?;
            (
                DiagnosticCaptureRef {
                    capture: validated.manifest.capture.clone(),
                    manifest_sha256: validated.manifest_sha.clone(),
                    source_scope_id: validated.range()?.scope_id,
                },
                validated.manifest.description.clone().ok_or_else(integrity)?,
                self.exported_rows(&validated)?,
            )
        };
        if parent != reference.capture_ref() {
            return Err(integrity());
        }
        row_references(&parent, &source, rows)?
            .into_iter()
            .find(|row| row.reference == *reference)
            .map(|row| (source, row.row))
            .ok_or_else(integrity)
    }

    pub(in crate::outbox) fn verify_focus_parent(&mut self, focus: &ActivityWaitFocus) -> Result<()> {
        let (source, row) = self.diagnostic_reference(&focus.parent)?;
        let TelemetryRecord::Activity(original) = &row.record else {
            return Err(integrity());
        };
        if !focus.matches_parent(&source, original, &focus.parent) {
            return Err(integrity());
        }
        Ok(())
    }
    pub(in crate::outbox) fn restore_diagnostic_input(&mut self, input: RestoredInput) -> Result<()> {
        self.install_diagnostic_input(input, InputInstallation::Pending)
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
        let rows = row_references(&input.parent, &input.source, input.rows.clone())?;
        if rows.iter().any(|row| !row.row.record.is_valid()) {
            return Err(integrity());
        }
        let encoded = encode(&input.rows)?;
        if encoded.len() > MAX_RESTORED_INPUT_BYTES {
            return Err(problem(OutboxProblem::Capacity));
        }
        let existing: Option<StoredDiagnosticInput> = self
            .journal
            .conn
            .query_row(
                "SELECT parent_json,source_json,restored_json,restored_sha,evidence_only FROM diagnostic_inputs WHERE observation_id=?1",
                [&input.parent.capture.observation_id],
                |row| Ok(StoredDiagnosticInput {
                    parent: row.get(0)?,
                    source: row.get(1)?,
                    restored: row.get(2)?,
                    checksum: row.get(3)?,
                    evidence_only: row.get(4)?,
                }),
            )
            .optional()
            .map_err(storage)?;
        if let Some(stored) = &existing {
            if decode::<DiagnosticCaptureRef>(&stored.parent)? != input.parent
                || decode::<CollectionDescription>(&stored.source)? != input.source
            {
                return Err(integrity());
            }
            if let Some(existing_rows) = &stored.restored {
                if existing_rows != &encoded
                    || stored.checksum.as_deref() != Some(digest(existing_rows.as_bytes()).as_str())
                {
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
                    tx.commit().map_err(storage)?;
                }
                return Ok(());
            }
        } else if self.epoch.is_some() && installation == InputInstallation::Pending {
            return Err(DeliveryProblem::Conflict.error());
        }
        let charge = encoded.len() as u64 + super::super::CAPTURE_ACCOUNTING;
        let status = self.journal.status(1, None)?;
        if status.charged_bytes.checked_add(charge).is_none_or(|total| total > self.journal.limits.max_bytes) {
            return Err(problem(OutboxProblem::Capacity));
        }
        let tx = self.journal.conn.transaction().map_err(storage)?;
        if existing.is_some() {
            tx.execute(
                "UPDATE diagnostic_inputs SET restored_json=?1,charged_bytes=?2,restored_sha=?4 WHERE observation_id=?3",
                params![encoded, super::super::journal::bytes_to_sql(charge)?, input.parent.capture.observation_id,digest(encoded.as_bytes())],
            )
            .map_err(storage)?;
        } else {
            tx.execute(
                "INSERT INTO diagnostic_inputs VALUES(?1,?2,?3,?4,?5,?9,?6,?7,?8,?9)",
                params![
                    input.parent.capture.observation_id,
                    DIAGNOSTIC_CONTROLLER_VERSION,
                    encode(&input.parent)?,
                    encode(&input.source)?,
                    input.connection,
                    encoded,
                    super::super::journal::bytes_to_sql(charge)?,
                    digest(encoded.as_bytes()),
                    installation == InputInstallation::EvidenceOnly
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
        tx.commit().map_err(storage)
    }
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
