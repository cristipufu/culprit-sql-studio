//! Verified incident checkpoints, installed before collection. SQLite owns references and quota reservations;
//! immutable private files hold the bounded native state. Unreferenced reservations are reclaimed after a crash.
use super::journal::{bytes_from_sql, bytes_to_sql};
use super::{Files, RevisionAuthority, problem, storage};
use crate::{
    collection_activity::ActivityBaseline, delivery::activity_recovery::RecoveredActivityCheckpoint, durable_fs,
};
use culprit_core::{Result, telemetry::*};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Read};

const MAX_BASELINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    source: CollectionDescription,
    capture: CaptureIdentity,
    store_id: String,
    manifest_digest: String,
    states: u32,
}

pub(super) fn charged_bytes(conn: &Connection) -> Result<u64> {
    conn.query_row("SELECT coalesce(sum(charged_bytes),0) FROM baseline_payloads", [], |row| row.get(0))
        .map_err(storage)
        .and_then(bytes_from_sql)
}

impl Files {
    pub(super) fn restore_activity(&mut self, checkpoint: RecoveredActivityCheckpoint) -> Result<()> {
        let (range, states) = checkpoint.into_parts();
        let RevisionAuthority::Recovery { store_id } = &self.authority else {
            return Err(DeliveryProblem::InvalidContract.error());
        };
        if store_id != range.store_id() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let store_id = store_id.clone();
        if self.epoch.is_some() || !self.journal.unfinished()?.is_empty() || self.journal.debts()? > 0 {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        self.recover_baselines()?;
        let source_id = range.manifest().source.logical_source_id()?;
        let metadata = Metadata {
            source: range.manifest().source.clone(),
            capture: range.manifest().capture.clone(),
            store_id,
            manifest_digest: range.manifest_digest().into(),
            states: states.len() as u32,
        };
        if let Some((before, _, _)) = self.baseline_metadata(&source_id)? {
            match compare(&metadata.capture, &before.capture)? {
                std::cmp::Ordering::Less => return Ok(()),
                std::cmp::Ordering::Equal => {
                    if metadata.manifest_digest != before.manifest_digest {
                        return Err(problem(OutboxProblem::Integrity));
                    }
                    self.restored_baseline(&metadata.source)?;
                    return Ok(());
                }
                std::cmp::Ordering::Greater => (),
            }
        }
        let bytes =
            durable_fs::json_bytes(&states, MAX_BASELINE_BYTES).map_err(|_| problem(OutboxProblem::Capacity))?;
        let payload = digest(&bytes);
        let encoded = serde_json::to_string(&metadata).map_err(|_| problem(OutboxProblem::Integrity))?;
        if encoded.len() > MAX_METADATA_BYTES {
            return Err(problem(OutboxProblem::Capacity));
        }
        self.reserve_baseline(&source_id, &payload, bytes.len() as u64)?;
        let path = self.root.join("baselines").join(&payload);
        if path.try_exists().map_err(storage)? {
            self.read_baseline_payload(&payload, bytes.len() as u64)?;
        } else {
            durable_fs::publish(&path, &bytes).map_err(storage)?;
        }
        // A crash before this transaction leaves an unreferenced, charged payload, never a new incident head.
        self.journal.conn.execute(
            "INSERT INTO baseline_heads VALUES(?1,?2,?3) ON CONFLICT(source_id) DO UPDATE SET payload=excluded.payload,metadata=excluded.metadata",
            params![source_id, payload, encoded],
        ).map_err(storage)?;
        self.recover_baselines()
    }

    fn reserve_baseline(&mut self, source: &str, payload: &str, bytes: u64) -> Result<()> {
        let tx = self.journal.conn.transaction().map_err(storage)?;
        let exists: bool = tx
            .query_row("SELECT EXISTS(SELECT 1 FROM baseline_payloads WHERE id=?1)", [payload], |r| r.get(0))
            .map_err(storage)?;
        let count: u32 = tx
            .query_row("SELECT count(*) FROM baseline_heads WHERE source_id<>?1", [source], |r| r.get(0))
            .map_err(storage)?;
        if count >= MAX_FLEET_TARGETS as u32 {
            return Err(problem(OutboxProblem::Capacity));
        }
        if exists {
            return Ok(());
        }
        let captures: i64 =
            tx.query_row("SELECT coalesce(sum(charged_bytes),0) FROM captures", [], |r| r.get(0)).map_err(storage)?;
        // Reserve both final and temporary files before writing. Retain that allowance until payload deletion.
        let charge =
            bytes.checked_mul(2).and_then(|n| n.checked_add(4096)).ok_or_else(|| problem(OutboxProblem::Capacity))?;
        if bytes_from_sql(captures)?
            .checked_add(charged_bytes(&tx)?)
            .and_then(|n| n.checked_add(charge))
            .is_none_or(|n| n > self.journal.limits.max_bytes)
        {
            return Err(problem(OutboxProblem::Capacity));
        }
        tx.execute(
            "INSERT INTO baseline_payloads VALUES(?1,?2,?3)",
            params![payload, bytes_to_sql(bytes)?, bytes_to_sql(charge)?],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    fn baseline_metadata(&self, source_id: &str) -> Result<Option<(Metadata, String, u64)>> {
        let row: Option<(String,String,i64)> = self.journal.conn.query_row(
            "SELECT h.metadata,p.id,p.bytes FROM baseline_heads h JOIN baseline_payloads p ON h.payload=p.id WHERE h.source_id=?1",
            [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional().map_err(storage)?;
        row.map(|(metadata, payload, bytes)| {
            if metadata.len() > MAX_METADATA_BYTES {
                return Err(problem(OutboxProblem::Integrity));
            }
            Ok((
                serde_json::from_str(&metadata).map_err(|_| problem(OutboxProblem::Integrity))?,
                payload,
                bytes_from_sql(bytes)?,
            ))
        })
        .transpose()
    }

    pub(super) fn restored_baseline(&self, source: &CollectionDescription) -> Result<Option<ActivityBaseline>> {
        let Some((metadata, payload, bytes)) = self.baseline_metadata(&source.logical_source_id()?)? else {
            return Ok(None);
        };
        if !matches!(&self.authority, RevisionAuthority::Recovery{store_id} if *store_id==metadata.store_id)
            || !valid_digest(&metadata.manifest_digest)
            || metadata.source.logical_source_id()? != source.logical_source_id()?
        {
            return Err(problem(OutboxProblem::Integrity));
        }
        let states: Vec<ActivityIncidentState> = serde_json::from_slice(&self.read_baseline_payload(&payload, bytes)?)
            .map_err(|_| problem(OutboxProblem::Integrity))?;
        let snapshot = metadata.source.observation.activity().ok_or_else(|| problem(OutboxProblem::Integrity))?;
        if states.len() != metadata.states as usize
            || states.len() < snapshot.qualifying as usize
            || states.iter().any(|state| !state.is_valid_at(&snapshot.at))
        {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(Some(ActivityBaseline { source: metadata.source, capture: metadata.capture, states }))
    }

    pub(super) fn validate_baselines(&self) -> Result<()> {
        let mut query = self.journal.conn.prepare("SELECT source_id,metadata FROM baseline_heads").map_err(storage)?;
        let mut rows = query.query([]).map_err(storage)?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(storage)? {
            count += 1;
            let id: String = row.get(0).map_err(storage)?;
            let text: String = row.get(1).map_err(storage)?;
            if count > MAX_FLEET_TARGETS || text.len() > MAX_METADATA_BYTES {
                return Err(problem(OutboxProblem::Integrity));
            }
            let metadata: Metadata = serde_json::from_str(&text).map_err(|_| problem(OutboxProblem::Integrity))?;
            if metadata.source.logical_source_id()? != id || self.restored_baseline(&metadata.source)?.is_none() {
                return Err(problem(OutboxProblem::Integrity));
            }
        }
        Ok(())
    }

    fn read_baseline_payload(&self, payload: &str, bytes: u64) -> Result<Vec<u8>> {
        if !valid_digest(payload) || bytes > MAX_BASELINE_BYTES as u64 {
            return Err(problem(OutboxProblem::Integrity));
        }
        let mut body = Vec::new();
        durable_fs::regular_file(&self.root.join("baselines").join(payload))
            .map_err(storage)?
            .take(bytes + 1)
            .read_to_end(&mut body)
            .map_err(storage)?;
        if body.len() as u64 != bytes || digest(&body) != payload {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(body)
    }

    pub(super) fn recover_baselines(&mut self) -> Result<()> {
        let rows: BTreeMap<String, bool> = {
            let mut query = self
                .journal
                .conn
                .prepare(
                    "SELECT p.id,EXISTS(SELECT 1 FROM baseline_heads h WHERE h.payload=p.id) FROM baseline_payloads p",
                )
                .map_err(storage)?;
            query
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(storage)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage)?
        };
        if rows.len() > MAX_FLEET_TARGETS + 1 || rows.keys().any(|id| !valid_digest(id)) {
            return Err(problem(OutboxProblem::Integrity));
        }
        let directory = self.root.join("baselines");
        if !fs::symlink_metadata(&directory).map_err(storage)?.file_type().is_dir() {
            return Err(problem(OutboxProblem::Integrity));
        }
        let mut count = 0;
        for file in fs::read_dir(&directory).map_err(storage)? {
            count += 1;
            let file = file.map_err(storage)?;
            let name = file.file_name();
            let name = name.to_str().ok_or_else(|| problem(OutboxProblem::Integrity))?;
            let (id, temporary) =
                name.strip_prefix('.').and_then(|s| s.strip_suffix(".new")).map(|s| (s, true)).unwrap_or((name, false));
            let referenced = rows.get(id).ok_or_else(|| problem(OutboxProblem::Integrity))?;
            if count > 2 * (MAX_FLEET_TARGETS + 1) || !file.file_type().map_err(storage)?.is_file() {
                return Err(problem(OutboxProblem::Integrity));
            }
            if !referenced || temporary {
                fs::remove_file(file.path()).map_err(storage)?;
            }
        }
        durable_fs::sync_directory(&directory).map_err(storage)?;
        self.journal.conn.execute("DELETE FROM baseline_payloads WHERE NOT EXISTS(SELECT 1 FROM baseline_heads h WHERE h.payload=baseline_payloads.id)",[]).map_err(storage)?;
        Ok(())
    }
}

pub(super) fn compare(a: &CaptureIdentity, b: &CaptureIdentity) -> Result<std::cmp::Ordering> {
    let (Some(a), Some(b)) = (&a.revision, &b.revision) else {
        return Err(problem(OutboxProblem::Integrity));
    };
    if a.outbox_id != b.outbox_id {
        return Err(DeliveryProblem::Conflict.error());
    }
    fn position(r: &CaptureRevision) -> Result<(u64, u64)> {
        let epoch = r.owner_epoch.parse().ok().filter(|n| *n > 0).ok_or_else(|| problem(OutboxProblem::Integrity))?;
        let sequence = r.sequence.parse().ok().filter(|n| *n > 0).ok_or_else(|| problem(OutboxProblem::Integrity))?;
        Ok((epoch, sequence))
    }
    Ok(position(a)?.cmp(&position(b)?))
}
