//! SQLite owns revision allocation, artifact receipts and exact-range heads in one transaction.
use super::{
    CAPTURE_ACCOUNTING, CAPTURE_ALLOWANCE, artifacts::ValidatedCapture, problem, storage, valid_id, validate_limits,
};
use crate::durable_fs;
use culprit_core::{Result, telemetry::*};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use std::{collections::BTreeSet, path::Path};

const JOURNAL_VERSION: i64 = 8;
const APPLICATION_ID: i64 = 0x43554c54;
const JOURNAL_PAGES: i64 = 4096;
const REQUEST_LIMIT: usize = 4096;

pub(super) struct Journal {
    pub conn: Connection,
    pub id: String,
    pub limits: OutboxLimits,
}
#[derive(Clone)]
pub(super) struct Capture {
    pub identity: CaptureIdentity,
    pub connection: String,
    pub request: CollectionRequest,
    pub state: OutboxCaptureState,
    pub source_work: SourceWorkState,
    pub charged_bytes: u64,
    pub problem: Option<OutboxProblem>,
}

impl Journal {
    pub(super) fn begin_remote_epoch(&mut self, epoch: u64, high_sequence: u64) -> Result<()> {
        let epoch = i64::try_from(epoch).map_err(|_| problem(OutboxProblem::Integrity))?;
        let high_sequence = i64::try_from(high_sequence).map_err(|_| problem(OutboxProblem::Integrity))?;
        let tx = self.conn.transaction().map_err(storage)?;
        let (previous_epoch, previous_sequence): (i64, i64) = tx
            .query_row("SELECT owner_epoch,sequence FROM metadata", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?;
        if epoch <= previous_epoch || high_sequence == i64::MAX {
            return Err(DeliveryProblem::Fenced.error());
        }
        tx.execute(
            "UPDATE metadata SET owner_epoch=?1,sequence=?2",
            params![epoch, high_sequence.max(previous_sequence)],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }
    pub(super) fn create(path: &Path, id: &str, limits: OutboxLimits) -> Result<Self> {
        durable_fs::create_file(path).map_err(storage)?.sync_all().map_err(storage)?;
        let conn = open(path, true)?;
        configure_owner(&conn)?;
        conn.execute_batch("PRAGMA application_id=1129663572; PRAGMA user_version=8;
          CREATE TABLE metadata(id TEXT PRIMARY KEY, limits_json TEXT NOT NULL, owner_epoch INTEGER NOT NULL, sequence INTEGER NOT NULL);
          CREATE TABLE captures(observation_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL,sequence INTEGER NOT NULL,
            connection TEXT NOT NULL,collection TEXT NOT NULL,request_json TEXT NOT NULL CHECK(length(request_json)<=4096),state TEXT NOT NULL,
            source_work TEXT NOT NULL,charged_bytes INTEGER NOT NULL CHECK(charged_bytes>=0),problem TEXT,
            manifest_sha TEXT,range_json TEXT,archive_range TEXT,archive_digest TEXT, UNIQUE(epoch,sequence));
          CREATE TABLE batches(batch_id TEXT PRIMARY KEY,observation_id TEXT NOT NULL REFERENCES captures(observation_id),
            stream TEXT NOT NULL,file TEXT NOT NULL,sha TEXT NOT NULL,rows INTEGER NOT NULL,bytes INTEGER NOT NULL);
          CREATE TABLE range_heads(range_key TEXT PRIMARY KEY,observation_id TEXT NOT NULL REFERENCES captures(observation_id));
          CREATE TABLE diagnostic_inputs(observation_id TEXT PRIMARY KEY,controller_version INTEGER NOT NULL,parent_json TEXT NOT NULL,source_json TEXT NOT NULL,connection TEXT NOT NULL,acknowledged INTEGER NOT NULL CHECK(acknowledged IN(0,1)),restored_json TEXT,charged_bytes INTEGER NOT NULL CHECK(charged_bytes>=0),restored_sha TEXT,evidence_only INTEGER NOT NULL DEFAULT 0 CHECK(evidence_only IN(0,1)));
          CREATE TABLE diagnostic_heads(controller_key TEXT PRIMARY KEY,controller_version INTEGER NOT NULL,observation_id TEXT NOT NULL,checkpoint_json TEXT NOT NULL,capture_json TEXT NOT NULL);
          CREATE TABLE diagnostic_episodes(controller_key TEXT NOT NULL,episode_id TEXT NOT NULL,revision TEXT NOT NULL,episode_json TEXT NOT NULL,PRIMARY KEY(controller_key,episode_id));
          CREATE TABLE diagnostic_pins(owner TEXT NOT NULL,observation_id TEXT NOT NULL,PRIMARY KEY(owner,observation_id));
          CREATE INDEX diagnostic_pin_capture ON diagnostic_pins(observation_id);
          CREATE TABLE diagnostic_steps(observation_id TEXT PRIMARY KEY,input_id TEXT NOT NULL,controller_key TEXT NOT NULL,expected_head TEXT,step_id TEXT NOT NULL,update_json TEXT NOT NULL,result_json TEXT,UNIQUE(input_id,step_id));
          CREATE INDEX capture_state ON captures(state);
          CREATE INDEX capture_collection ON captures(connection,collection,state,epoch DESC,sequence DESC);") .map_err(storage)?;
        conn.execute("INSERT INTO metadata VALUES(?1,?2,0,0)", params![id, encode(&limits)?]).map_err(storage)?;
        durable_fs::sync_directory(path.parent().ok_or_else(|| problem(OutboxProblem::Storage))?).map_err(storage)?;
        Ok(Self { conn, id: id.into(), limits })
    }
    fn release_terminal_focus(tx: &Transaction<'_>, id: &str) -> Result<()> {
        tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1 AND NOT EXISTS(SELECT 1 FROM captures WHERE observation_id=?2 AND source_work IN(?3,?4))",params![format!("focus:{id}"),id,encode(&SourceWorkState::Active)?,encode(&SourceWorkState::Unconfirmed)?]).map_err(storage)?;
        Ok(())
    }
    pub(super) fn open(path: &Path, id: &str, owner: bool) -> Result<Self> {
        let conn = open(path, owner)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(storage)?;
        let app: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0)).map_err(storage)?;
        if version != JOURNAL_VERSION || app != APPLICATION_ID {
            return Err(problem(OutboxProblem::UnsupportedFormat));
        }
        let identities: u32 = conn.query_row("SELECT count(*) FROM metadata", [], |row| row.get(0)).map_err(storage)?;
        if identities != 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        let (actual, limits): (String, String) = conn
            .query_row("SELECT id,limits_json FROM metadata", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?;
        if actual != id {
            return Err(problem(OutboxProblem::Integrity));
        }
        let limits: OutboxLimits = decode(&limits)?;
        validate_limits(&limits)?;
        if owner {
            configure_owner(&conn)?;
        }
        Ok(Self { conn, id: id.into(), limits })
    }
    pub(super) fn reserve(
        &mut self,
        connection: &str,
        request: &CollectionRequest,
        owner_epoch: &mut Option<i64>,
    ) -> Result<Capture> {
        let charge = u64::from(request.budget.total_bytes) + CAPTURE_ALLOWANCE;
        let request_json = encode(request)?;
        if request_json.len() > REQUEST_LIMIT {
            return Err(problem(OutboxProblem::Integrity));
        }
        let tx = self.conn.transaction().map_err(storage)?;
        let (count, used): (u32, i64) = tx
            .query_row("SELECT count(*),coalesce(sum(charged_bytes),0) FROM captures", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(storage)?;
        let diagnostic_bytes = super::investigations::journal::charged_bytes(&tx)?;
        let used =
            bytes_from_sql(used)?.checked_add(diagnostic_bytes).ok_or_else(|| problem(OutboxProblem::Capacity))?;
        let debts: u32 = tx
            .query_row(
                "SELECT count(*) FROM captures WHERE source_work IN (?1,?2)",
                params![encode(&SourceWorkState::Active)?, encode(&SourceWorkState::Unconfirmed)?],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if debts > 0 && request.collection() != TelemetryCollection::Diagnostics {
            return Err(problem(OutboxProblem::SourceUnconfirmed));
        }
        let pending: u32 = tx
            .query_row(
                "SELECT count(*) FROM captures WHERE state=?1",
                [encode(&OutboxCaptureState::Capturing)?],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if pending > 0 {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        if count >= self.limits.max_captures
            || used.checked_add(charge).is_none_or(|bytes| bytes > self.limits.max_bytes)
        {
            return Err(problem(OutboxProblem::Capacity));
        }
        let (epoch, sequence): (i64, i64) = tx
            .query_row("SELECT owner_epoch,sequence FROM metadata", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?;
        let (epoch, sequence) = match owner_epoch {
            Some(owner) if *owner == epoch => {
                (epoch, sequence.checked_add(1).ok_or_else(|| problem(OutboxProblem::Integrity))?)
            }
            Some(_) => return Err(problem(OutboxProblem::Integrity)),
            None => (epoch.checked_add(1).ok_or_else(|| problem(OutboxProblem::Integrity))?, 1),
        };
        if epoch <= 0 || sequence <= 0 {
            return Err(problem(OutboxProblem::Integrity));
        }
        let identity = CaptureIdentity {
            observation_id: durable_fs::random_id()?,
            revision: Some(CaptureRevision {
                outbox_id: self.id.clone(),
                owner_epoch: epoch.to_string(),
                sequence: sequence.to_string(),
            }),
        };
        tx.execute("UPDATE metadata SET owner_epoch=?1,sequence=?2", params![epoch, sequence]).map_err(storage)?;
        let source_work = if request.collection() == TelemetryCollection::Diagnostics {
            SourceWorkState::Quiescent
        } else {
            SourceWorkState::NotStarted
        };
        tx.execute("INSERT INTO captures(observation_id,epoch,sequence,connection,request_json,state,source_work,charged_bytes,collection)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![identity.observation_id,epoch,sequence,connection,request_json,
            encode(&OutboxCaptureState::Capturing)?,encode(&source_work)?,bytes_to_sql(charge)?,encode(&request.collection())?]).map_err(storage)?;
        if let CollectionSelection::FocusedPageWait { focus } = &request.selection {
            tx.execute(
                "INSERT INTO diagnostic_pins VALUES(?1,?2)",
                params![format!("focus:{}", identity.observation_id), focus.parent.capture.observation_id],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        *owner_epoch = Some(epoch);
        Ok(Capture {
            identity,
            connection: connection.into(),
            request: request.clone(),
            state: OutboxCaptureState::Capturing,
            source_work,
            charged_bytes: charge,
            problem: None,
        })
    }
    pub(super) fn capture(&self, id: &str) -> Result<Capture> {
        if !valid_id(id) {
            return Err(problem(OutboxProblem::MissingCapture));
        }
        let mut stmt=self.conn.prepare("SELECT epoch,sequence,connection,request_json,state,source_work,charged_bytes,problem FROM captures WHERE observation_id=?1").map_err(storage)?;
        let mut rows = stmt.query([id]).map_err(storage)?;
        let row = rows.next().map_err(storage)?.ok_or_else(|| problem(OutboxProblem::MissingCapture))?;
        self.row_capture(id, row)
    }
    fn row_capture(&self, id: &str, row: &rusqlite::Row<'_>) -> Result<Capture> {
        let epoch: i64 = row.get(0).map_err(storage)?;
        let sequence: i64 = row.get(1).map_err(storage)?;
        if epoch <= 0 || sequence <= 0 {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(Capture {
            identity: CaptureIdentity {
                observation_id: id.into(),
                revision: Some(CaptureRevision {
                    outbox_id: self.id.clone(),
                    owner_epoch: epoch.to_string(),
                    sequence: sequence.to_string(),
                }),
            },
            connection: row.get(2).map_err(storage)?,
            request: decode(&row.get::<_, String>(3).map_err(storage)?)?,
            state: decode(&row.get::<_, String>(4).map_err(storage)?)?,
            source_work: decode(&row.get::<_, String>(5).map_err(storage)?)?,
            charged_bytes: bytes_from_sql(row.get(6).map_err(storage)?)?,
            problem: row.get::<_, Option<String>>(7).map_err(storage)?.map(|text| decode(&text)).transpose()?,
        })
    }
    pub(super) fn source_work(&mut self, capture: &Capture, state: SourceWorkState) -> Result<()> {
        let previous =
            if state == SourceWorkState::Active { SourceWorkState::NotStarted } else { SourceWorkState::Active };
        let updated = self
            .conn
            .execute(
                "UPDATE captures SET source_work=?1 WHERE observation_id=?2 AND state=?3 AND source_work=?4",
                params![
                    encode(&state)?,
                    capture.identity.observation_id,
                    encode(&OutboxCaptureState::Capturing)?,
                    encode(&previous)?
                ],
            )
            .map_err(storage)?;
        if updated != 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(())
    }
    pub(super) fn prepare_commit<'a>(
        &'a mut self,
        capture: &Capture,
        validated: &ValidatedCapture,
    ) -> Result<Transaction<'a>> {
        let current = self.capture(&capture.identity.observation_id)?;
        if current.identity != capture.identity
            || current.state != OutboxCaptureState::Capturing
            || current.source_work != SourceWorkState::Quiescent
        {
            return Err(problem(OutboxProblem::Integrity));
        }
        let range = validated.range()?;
        let tx = self.conn.transaction().map_err(storage)?;
        for file in &validated.manifest.files {
            tx.execute(
                "INSERT INTO batches VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    file.batch_id,
                    capture.identity.observation_id,
                    encode(&file.stream)?,
                    file.file,
                    file.sha256,
                    file.rows,
                    file.bytes
                ],
            )
            .map_err(storage)?;
        }
        tx.execute(
            "UPDATE captures SET state=?1,charged_bytes=?2,manifest_sha=?3,range_json=?4 WHERE observation_id=?5",
            params![
                encode(&OutboxCaptureState::Ready)?,
                bytes_to_sql(validated.bytes + CAPTURE_ACCOUNTING)?,
                validated.manifest_sha,
                encode(&range)?,
                capture.identity.observation_id
            ],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO range_heads VALUES(?1,?2) ON CONFLICT(range_key) DO UPDATE SET observation_id=excluded.observation_id",
            params![range_key(&range)?,capture.identity.observation_id]).map_err(storage)?;
        super::investigations::journal::prepare_commit(&tx, capture, validated)?;
        Ok(tx)
    }
    pub(super) fn check_receipt(&self, capture: &Capture, validated: &ValidatedCapture) -> Result<()> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT manifest_sha FROM captures WHERE observation_id=?1",
                [&capture.identity.observation_id],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if stored.as_deref() != Some(&validated.manifest_sha) {
            return Err(problem(OutboxProblem::Integrity));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT batch_id,stream,file,sha,rows,bytes FROM batches WHERE observation_id=?1 ORDER BY stream")
            .map_err(storage)?;
        let mut rows = stmt.query([&capture.identity.observation_id]).map_err(storage)?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(storage)? {
            count += 1;
            let batch: String = row.get(0).map_err(storage)?;
            let expected = validated
                .manifest
                .files
                .iter()
                .find(|file| file.batch_id == batch)
                .ok_or_else(|| problem(OutboxProblem::Integrity))?;
            if row.get::<_, String>(1).map_err(storage)? != encode(&expected.stream)?
                || row.get::<_, String>(2).map_err(storage)? != expected.file
                || row.get::<_, String>(3).map_err(storage)? != expected.sha256.clone().unwrap_or_default()
                || row.get::<_, u32>(4).map_err(storage)? != expected.rows
                || row.get::<_, u32>(5).map_err(storage)? != expected.bytes
            {
                return Err(problem(OutboxProblem::Integrity));
            }
        }
        if count != validated.manifest.files.len() {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(())
    }
    pub(super) fn fail(
        &mut self,
        capture: &Capture,
        state: OutboxCaptureState,
        reason: Option<OutboxProblem>,
        bytes: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction().map_err(storage)?;
        tx.execute("DELETE FROM range_heads WHERE observation_id=?1", [&capture.identity.observation_id])
            .map_err(storage)?;
        tx.execute(
            "UPDATE captures SET state=?1,problem=?2,charged_bytes=?3 WHERE observation_id=?4",
            params![
                encode(&state)?,
                reason.map(|reason| encode(&reason)).transpose()?,
                bytes_to_sql(bytes)?,
                capture.identity.observation_id
            ],
        )
        .map_err(storage)?;
        if matches!(
            state,
            OutboxCaptureState::Incomplete | OutboxCaptureState::Interrupted | OutboxCaptureState::Quarantined
        ) {
            Self::release_terminal_focus(&tx, &capture.identity.observation_id)?;
        }
        tx.commit().map_err(storage)
    }
    pub(super) fn acknowledge(&mut self, id: &str) -> Result<()> {
        let capture = self.capture(id)?;
        if capture.state == OutboxCaptureState::Capturing {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        if !matches!(capture.source_work, SourceWorkState::Active | SourceWorkState::Unconfirmed) {
            return Err(problem(OutboxProblem::Integrity));
        }
        let tx = self.conn.transaction().map_err(storage)?;
        tx.execute(
            "UPDATE captures SET source_work=?1 WHERE observation_id=?2",
            params![encode(&SourceWorkState::Quiescent)?, id],
        )
        .map_err(storage)?;
        Self::release_terminal_focus(&tx, id)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    pub(super) fn ids(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare("SELECT observation_id FROM captures LIMIT ?1").map_err(storage)?;
        let ids = stmt
            .query_map([self.limits.max_captures + 1], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .map_err(storage)?;
        if ids.len() > self.limits.max_captures as usize || ids.iter().any(|id| !valid_id(id)) {
            return Err(problem(OutboxProblem::Integrity));
        }
        Ok(ids)
    }
    pub(super) fn unfinished(&self) -> Result<Vec<Capture>> {
        let mut stmt =
            self.conn.prepare("SELECT observation_id FROM captures WHERE state=?1 LIMIT 2").map_err(storage)?;
        let ids = stmt
            .query_map([encode(&OutboxCaptureState::Capturing)?], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        if ids.len() > 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        ids.iter().map(|id| self.capture(id)).collect()
    }
    pub(super) fn debts(&self) -> Result<u32> {
        self.conn
            .query_row(
                "SELECT count(*) FROM captures WHERE source_work IN (?1,?2)",
                params![encode(&SourceWorkState::Active)?, encode(&SourceWorkState::Unconfirmed)?],
                |row| row.get(0),
            )
            .map_err(storage)
    }
    pub(super) fn head(&self, key: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        let id: Option<String> = self
            .conn
            .query_row("SELECT observation_id FROM range_heads WHERE range_key=?1", [range_key(key)?], |row| row.get(0))
            .optional()
            .map_err(storage)?;
        id.map(|id| self.capture(&id).map(|capture| capture.identity)).transpose()
    }
    pub(super) fn status(&self, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        if !(1..=500).contains(&limit) || cursor.is_some_and(|cursor| cursor.len() > 128) {
            return Err(problem(OutboxProblem::InvalidLimits));
        }
        let after = cursor.map(decode::<(i64, i64)>).transpose()?.unwrap_or((i64::MAX, i64::MAX));
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        let (captures, charged_bytes): (u32, i64) = tx
            .query_row("SELECT count(*),coalesce(sum(charged_bytes),0) FROM captures", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(storage)?;
        let diagnostic_bytes = super::investigations::journal::charged_bytes(&tx)?;
        let charged_bytes = bytes_from_sql(charged_bytes)?
            .checked_add(diagnostic_bytes)
            .ok_or_else(|| problem(OutboxProblem::Integrity))?;
        let ready: u32 = tx
            .query_row("SELECT count(*) FROM captures WHERE state=?1", [encode(&OutboxCaptureState::Ready)?], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        let unfinished: u32 = tx
            .query_row(
                "SELECT count(*) FROM captures WHERE state=?1",
                [encode(&OutboxCaptureState::Capturing)?],
                |row| row.get(0),
            )
            .map_err(storage)?;
        let mut stmt=tx.prepare("SELECT observation_id,epoch,sequence,connection,request_json,state,source_work,charged_bytes,problem FROM captures
            WHERE (epoch,sequence)<(?1,?2) ORDER BY epoch DESC,sequence DESC LIMIT ?3").map_err(storage)?;
        let mut rows = stmt.query(params![after.0, after.1, limit + 1]).map_err(storage)?;
        let mut entries = Vec::new();
        let mut last = None;
        let mut has_more = false;
        while let Some(row) = rows.next().map_err(storage)? {
            if entries.len() == limit as usize {
                has_more = true;
                break;
            }
            let id: String = row.get(0).map_err(storage)?;
            let capture = self.capture(&id)?;
            last = Some((row.get::<_, i64>(1).map_err(storage)?, row.get::<_, i64>(2).map_err(storage)?));
            entries.push(OutboxEntry {
                capture: capture.identity,
                connection: capture.connection,
                request: capture.request,
                state: capture.state,
                source_work: capture.source_work,
                charged_bytes: capture.charged_bytes,
                problem: capture.problem,
            });
        }
        Ok(OutboxStatus {
            outbox_id: self.id.clone(),
            limits: self.limits.clone(),
            captures,
            ready,
            source_debts: self.debts()?,
            unconfirmed_sources: tx
                .query_row(
                    "SELECT count(*) FROM captures WHERE source_work=?1",
                    [encode(&SourceWorkState::Unconfirmed)?],
                    |row| row.get(0),
                )
                .map_err(storage)?,
            unfinished,
            charged_bytes,
            entries,
            next_cursor: if has_more { last.map(|cursor| encode(&cursor)).transpose()? } else { None },
        })
    }
}
fn open(path: &Path, owner: bool) -> Result<Connection> {
    durable_fs::regular_file(path).map_err(storage)?;
    let flags = if owner { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(storage)?;
    conn.busy_timeout(std::time::Duration::from_millis(250)).map_err(storage)?;
    let pages: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0)).map_err(storage)?;
    if pages > JOURNAL_PAGES {
        return Err(problem(OutboxProblem::Integrity));
    }

    Ok(conn)
}
fn configure_owner(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
          PRAGMA wal_autocheckpoint=128; PRAGMA journal_size_limit=2097152; PRAGMA cache_size=-2048;
          PRAGMA mmap_size=0; PRAGMA trusted_schema=OFF;",
    )
    .map_err(storage)?;
    let limit: i64 =
        conn.pragma_update_and_check(None, "max_page_count", JOURNAL_PAGES, |row| row.get(0)).map_err(storage)?;
    let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0)).map_err(storage)?;
    let sync: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0)).map_err(storage)?;
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0)).map_err(storage)?;
    if mode != "wal" || sync != 2 || limit != JOURNAL_PAGES || page_size != 4096 {
        return Err(problem(OutboxProblem::Storage));
    }
    Ok(())
}

pub(super) fn encode(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| problem(OutboxProblem::Integrity))
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| problem(OutboxProblem::Integrity))
}
fn range_key(range: &CapturedRange) -> Result<String> {
    Ok(digest(encode(&(&range.scope_id, range.range.canonical()?))?.as_bytes()))
}
pub(super) fn bytes_to_sql(bytes: u64) -> Result<i64> {
    bytes.try_into().map_err(|_| problem(OutboxProblem::Integrity))
}
pub(super) fn bytes_from_sql(bytes: i64) -> Result<u64> {
    bytes.try_into().map_err(|_| problem(OutboxProblem::Integrity))
}
