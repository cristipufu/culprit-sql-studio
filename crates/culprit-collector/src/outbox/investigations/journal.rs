use super::*;

pub(in crate::outbox) fn prepare_commit(
    tx: &Transaction<'_>,
    capture: &super::super::journal::Capture,
    validated: &ValidatedCapture,
) -> Result<()> {
    let source = validated.manifest.description.as_ref().ok_or_else(integrity)?;
    let parent = DiagnosticCaptureRef {
        capture: capture.identity.clone(),
        manifest_sha256: validated.manifest_sha.clone(),
        source_scope_id: source.captured_range()?.scope_id,
    };
    if eligible(&capture.request) {
        tx.execute("INSERT INTO diagnostic_inputs(observation_id,controller_version,parent_json,source_json,connection,acknowledged,restored_json,charged_bytes) VALUES(?1,?2,?3,?4,?5,0,NULL,0)",
            params![capture.identity.observation_id, DIAGNOSTIC_CONTROLLER_VERSION, encode(&parent)?, encode(source)?, capture.connection]).map_err(storage)?;
        pin(tx, &format!("input:{}", capture.identity.observation_id), &capture.identity.observation_id)?;
    }
    if capture.request.collection() == TelemetryCollection::Diagnostics {
        complete_step(tx, capture, validated)?;
    }
    Ok(())
}

fn complete_step(
    tx: &Transaction<'_>,
    capture: &super::super::journal::Capture,
    validated: &ValidatedCapture,
) -> Result<()> {
    let (input_id, scope, expected, update, update_digest): (String, String, Option<String>, String, String) = tx
        .query_row(
            "SELECT input_id,controller_key,expected_head,update_json,update_digest FROM diagnostic_steps WHERE observation_id=?1",
            [&capture.identity.observation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .map_err(storage)?;
    let update: DiagnosticUpdate = decode(&update)?;
    let checkpoint = validated.diagnostic_checkpoint.as_ref().ok_or_else(integrity)?;
    if checkpoint != &update.checkpoint
        || validated.diagnostic_revisions != update.revisions
        || update.replay_digest()? != update_digest
    {
        return Err(integrity());
    }
    let current: Option<String> = tx
        .query_row("SELECT observation_id FROM diagnostic_heads WHERE controller_key=?1", [&scope], |row| row.get(0))
        .optional()
        .map_err(storage)?;
    if current != expected {
        return Err(DeliveryProblem::Conflict.error());
    }
    validate_transition(tx, &scope, &update)?;
    let head_owner = format!("head:{scope}");
    tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [&head_owner]).map_err(storage)?;
    for reference in checkpoint_references(checkpoint) {
        pin(tx, &head_owner, &reference.capture.observation_id)?;
    }
    for episode in &checkpoint.episodes {
        if let Some(recovery) = &episode.recovery {
            pin(tx, &head_owner, &recovery.capture().capture.observation_id)?;
        }
    }
    pin(tx, &head_owner, &capture.identity.observation_id)?;
    let result = DiagnosticCaptureRef {
        capture: capture.identity.clone(),
        manifest_sha256: validated.manifest_sha.clone(),
        source_scope_id: validated.range()?.scope_id,
    };
    tx.execute("INSERT INTO diagnostic_heads VALUES(?1,?2,?3,?4,?5) ON CONFLICT(controller_key) DO UPDATE SET controller_version=excluded.controller_version,observation_id=excluded.observation_id,checkpoint_json=excluded.checkpoint_json,capture_json=excluded.capture_json",
        params![scope,checkpoint.controller_version, capture.identity.observation_id, encode(checkpoint)?,encode(&result)?]).map_err(storage)?;
    tx.execute("DELETE FROM diagnostic_episodes WHERE controller_key=?1", [&scope]).map_err(storage)?;
    for episode in &checkpoint.episodes {
        tx.execute(
            "INSERT INTO diagnostic_episodes VALUES(?1,?2,?3,?4)",
            params![scope, episode.id, episode.revision, encode(episode)?],
        )
        .map_err(storage)?;
    }
    let derived_owner = format!("derived:{}", capture.identity.observation_id);
    for reference in update.revisions.iter().flat_map(|revision| &revision.inputs) {
        pin(tx, &derived_owner, &reference.capture.observation_id)?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO diagnostic_pins SELECT ?1,observation_id FROM diagnostic_pins WHERE owner=?2",
        params![derived_owner, format!("focus:{input_id}")],
    )
    .map_err(storage)?;
    tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [format!("focus:{input_id}")]).map_err(storage)?;
    let acknowledged = tx
        .execute(
            "UPDATE diagnostic_inputs SET acknowledged=1,evidence_only=0 WHERE observation_id=?1 AND controller_version=?2",
            params![input_id, checkpoint.controller_version],
        )
        .map_err(storage)?;
    if acknowledged != 1 {
        return Err(integrity());
    }
    tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [format!("input:{input_id}")]).map_err(storage)?;
    tx.execute(
        "UPDATE diagnostic_steps SET result_json=?1 WHERE observation_id=?2",
        params![encode(&result)?, capture.identity.observation_id],
    )
    .map_err(storage)?;
    super::retirement::compact(tx)
}

pub(in crate::outbox) fn validate_transition(
    tx: &rusqlite::Connection,
    scope: &str,
    update: &DiagnosticUpdate,
) -> Result<()> {
    if !update.checkpoint.is_valid()
        || update.revisions.len() > MAX_DIAGNOSTIC_EPISODES
        || encode(&update.checkpoint)?.len() > MAX_CHECKPOINT_BYTES
    {
        return Err(integrity());
    }
    let mut previous = BTreeMap::new();
    let mut query = tx
        .prepare("SELECT episode_id,episode_json FROM diagnostic_episodes WHERE controller_key=?1 LIMIT ?2")
        .map_err(storage)?;
    let rows = query
        .query_map(params![scope, (MAX_DIAGNOSTIC_EPISODES + 1) as u32], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage)?;
    for row in rows {
        let (id, json) = row.map_err(storage)?;
        let episode: DiagnosticEpisode = decode(&json)?;
        if !episode.is_valid()
            || episode.id != id
            || previous.insert(id, episode).is_some()
            || previous.len() > MAX_DIAGNOSTIC_EPISODES
        {
            return Err(integrity());
        }
    }
    let mut changed = BTreeSet::new();
    for revision in &update.revisions {
        if !revision.is_valid()
            || revision.controller_version != update.checkpoint.controller_version
            || !changed.insert(&revision.episode.id)
        {
            return Err(integrity());
        }
        let prior = previous.get(&revision.episode.id);
        if prior.map(|episode| &episode.revision) != revision.previous_revision.as_ref() {
            return Err(DeliveryProblem::Conflict.error());
        }
        validate_investigation_transition(prior, revision)?;
        if let Some(prior) = prior
            && (revision.episode.first != prior.first
                || revision.episode.scope != prior.scope
                || revision.episode.signal != prior.signal
                || revision.episode.first_seen_at != prior.first_seen_at
                || ObservationBounds::new(&prior.last_seen_at, &revision.episode.last_seen_at).is_none()
                || (prior.lifecycle == DiagnosticLifecycle::Resolved
                    && (revision.episode.lifecycle != DiagnosticLifecycle::Resolved
                        || revision.episode.recovery != prior.recovery)))
        {
            return Err(integrity());
        }
        if revision.episode.retained_in_checkpoint() {
            previous.insert(revision.episode.id.clone(), revision.episode.clone());
        } else {
            previous.remove(&revision.episode.id);
        }
    }
    let next: BTreeMap<_, _> =
        update.checkpoint.episodes.iter().map(|episode| (episode.id.clone(), episode.clone())).collect();
    if previous != next {
        return Err(integrity());
    }
    Ok(())
}

fn validate_investigation_transition(prior: Option<&DiagnosticEpisode>, revision: &DiagnosticRevision) -> Result<()> {
    let previous_key = prior.and_then(|episode| episode.investigation_event.as_ref());
    let changed = previous_key != revision.episode.investigation_event.as_ref();
    if changed != revision.investigation.is_some()
        || revision
            .investigation
            .as_ref()
            .is_some_and(|event| prior.is_none_or(|episode| !event.matches_episode(episode)))
    {
        return Err(integrity());
    }
    Ok(())
}

pub(in crate::outbox) fn checkpoint_references(
    checkpoint: &DiagnosticCheckpoint,
) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
    checkpoint.episodes.iter().flat_map(|episode| {
        [&episode.first, &episode.latest]
            .into_iter()
            .chain(episode.peak.iter())
            .chain(episode.report.as_ref().map(DiagnosticReport::evidence))
    })
}
fn pin(tx: &Transaction<'_>, owner: &str, observation_id: &str) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO diagnostic_pins VALUES(?1,?2)", params![owner, observation_id])
        .map_err(storage)?;
    Ok(())
}
pub(in crate::outbox) fn charged_bytes(conn: &rusqlite::Connection) -> Result<u64> {
    let value: i64 = conn
        .query_row("SELECT coalesce(sum(charged_bytes),0) FROM diagnostic_inputs", [], |row| row.get(0))
        .map_err(storage)?;
    super::super::journal::bytes_from_sql(value)
}
pub(in crate::outbox) fn is_pinned(conn: &rusqlite::Connection, observation_id: &str) -> Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM diagnostic_pins WHERE observation_id=?1)", [observation_id], |row| {
        row.get(0)
    })
    .map_err(storage)
}
