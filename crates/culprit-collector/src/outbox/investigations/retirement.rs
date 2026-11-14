//! Archive acknowledgement retires local replay metadata; the recovery pending index owns old work admission.
use super::*;
use rusqlite::Connection;

pub(in crate::outbox) fn generation(conn: &Connection) -> Result<i64> {
    let value = conn
        .query_row("SELECT diagnostic_gc_generation FROM metadata", [], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    if value < 0 {
        return Err(integrity());
    }
    Ok(value)
}

/// This is called only in the transaction that records an exact verified archive/eviction receipt.
pub(in crate::outbox) fn archived(tx: &Transaction<'_>, observation_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE diagnostic_steps SET archived=1,update_json=NULL WHERE observation_id=?1 AND result_json IS NOT NULL",
        [observation_id],
    )
    .map_err(storage)?;
    compact(tx)
}

/// The current checkpoint and every queued/native/focus owner remain authoritative, including Deferred work.
pub(in crate::outbox) fn compact(tx: &Transaction<'_>) -> Result<()> {
    let protected = protected_captures(tx)?;
    let steps = {
        let mut query = tx
            .prepare("SELECT observation_id FROM diagnostic_steps WHERE archived=1 AND result_json IS NOT NULL")
            .map_err(storage)?;
        query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?
    };
    let mut changed = 0;
    for id in steps {
        if !protected.contains(&id) {
            changed += tx.execute("DELETE FROM diagnostic_steps WHERE observation_id=?1", [&id]).map_err(storage)?;
        }
    }
    let inputs = {
        let mut query = tx
            .prepare(
                "SELECT i.observation_id FROM diagnostic_inputs i
                 LEFT JOIN captures c ON c.observation_id=i.observation_id
                 WHERE i.acknowledged=1
                   AND NOT EXISTS(SELECT 1 FROM diagnostic_steps s WHERE s.input_id=i.observation_id)
                   AND (c.observation_id IS NULL OR (c.source_work=?1 AND c.archive_range IS NOT NULL))",
            )
            .map_err(storage)?;
        query
            .query_map([encode(&SourceWorkState::Quiescent)?], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?
    };
    for id in inputs {
        if !protected.contains(&id) {
            // Restored bytes and their exact quota charge retire in this same atomic row deletion.
            changed += tx.execute("DELETE FROM diagnostic_inputs WHERE observation_id=?1", [&id]).map_err(storage)?;
        }
    }
    if changed > 0 {
        let next = generation(tx)?.checked_add(1).ok_or_else(integrity)?;
        tx.execute("UPDATE metadata SET diagnostic_gc_generation=?1", [next]).map_err(storage)?;
    }
    Ok(())
}

fn protected_captures(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut protected = {
        let mut query = conn.prepare("SELECT DISTINCT observation_id FROM diagnostic_pins").map_err(storage)?;
        query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .map_err(storage)?
    };
    let mut debts = conn.prepare("SELECT observation_id FROM captures WHERE source_work IN(?1,?2)").map_err(storage)?;
    for id in debts
        .query_map(params![encode(&SourceWorkState::Active)?, encode(&SourceWorkState::Unconfirmed)?], |row| {
            row.get::<_, String>(0)
        })
        .map_err(storage)?
    {
        protected.insert(id.map_err(storage)?);
    }
    let mut query = conn.prepare("SELECT observation_id,checkpoint_json FROM diagnostic_heads").map_err(storage)?;
    let mut rows = query.query([]).map_err(storage)?;
    while let Some(row) = rows.next().map_err(storage)? {
        protected.insert(row.get(0).map_err(storage)?);
        let json: String = row.get(1).map_err(storage)?;
        if json.len() > MAX_CHECKPOINT_BYTES {
            return Err(integrity());
        }
        let checkpoint: DiagnosticCheckpoint = decode(&json)?;
        if !checkpoint.is_valid() {
            return Err(integrity());
        }
        protected.insert(checkpoint.input.capture.observation_id.clone());
        for reference in super::journal::checkpoint_references(&checkpoint) {
            protected.insert(reference.capture.observation_id.clone());
        }
        for episode in &checkpoint.episodes {
            if let Some(recovery) = &episode.recovery {
                protected.insert(recovery.capture().capture.observation_id.clone());
                if let DiagnosticRecoveryProof::FocusedParticipantGone { parent, .. } = recovery {
                    protected.insert(parent.capture.observation_id.clone());
                }
            }
        }
    }
    Ok(protected)
}
