//! Side effects have a durable intent. Notification cursors and the resulting work hints commit together.
use super::{contract::*, evidence::*, recovery::CommittedRange, sink::*};
use crate::{blocking_state::BlockingState, durable_fs};
use culprit_core::{
    Result,
    telemetry::{DeliveryBacklog, DeliveryEntry, DeliveryPhase, DeliveryProblem, digest},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_PENDING: u32 = 100_000;
const JOURNAL_PAGES: u32 = 131_072;
const MAX_UPDATE_PAGE: usize = 128;
// Keep room for mutable proof progress and bounded WAL work while target hydration is backpressured.
pub(super) const MAX_IMMUTABLE_BYTES: u64 = JOURNAL_PAGES as u64 * 4096 / 2;
const CURRENT_METADATA_RESERVE: u64 = 2 * MAX_MANIFEST_BYTES as u64;
const CURRENT_RANGE_RESERVE: u32 = 128;

/// Historical hydration cannot consume the capacity reserved for current observations.
#[derive(Clone, Copy)]
pub enum HydrationPriority {
    Historical,
    Current,
}
impl HydrationPriority {
    fn metadata_limit(self) -> u64 {
        match self {
            Self::Historical => MAX_IMMUTABLE_BYTES - CURRENT_METADATA_RESERVE,
            Self::Current => MAX_IMMUTABLE_BYTES,
        }
    }
    fn range_limit(self) -> u32 {
        match self {
            Self::Historical => MAX_PENDING - CURRENT_RANGE_RESERVE,
            Self::Current => MAX_PENDING,
        }
    }
}
const BACKLOG_QUERY: &str = "SELECT phase,count(*),min(captured_at) FROM deliveries GROUP BY phase";
fn storage(_: impl std::fmt::Display) -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}
fn integrity() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| integrity())
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|_| integrity())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub phase: DeliveryPhase,
    pub attempt: Option<DeliveryAttempt>,
    pub retry_at_ms: i64,
    pub visibility_deadline_ms: Option<i64>,
    pub expected_head: Option<String>,
    pub head_known: bool,
    pub reconcile: bool,
    pub problem: Option<DeliveryProblem>,
    pub verification: Option<super::verification::VerificationProgress>,
    pub staged: Vec<super::recovery::StagedBatch>,
    pub dispatched_batches: usize,
}
impl Default for State {
    fn default() -> Self {
        Self {
            phase: DeliveryPhase::Captured,
            attempt: None,
            retry_at_ms: 0,
            visibility_deadline_ms: None,
            expected_head: None,
            head_known: false,
            reconcile: false,
            problem: None,
            verification: None,
            staged: vec![],
            dispatched_batches: 0,
        }
    }
}
pub(crate) struct PendingRange {
    pub manifest: Arc<RangeManifest>,
    pub manifest_digest: String,
    pub manifest_bytes: usize,
    pub directory: PathBuf,
    pub state: State,
    pub version: i64,
}
impl PendingRange {
    pub fn id(&self) -> &str {
        &self.manifest.range_id
    }
    pub fn prepared(&self) -> PreparedRange {
        PreparedRange { manifest: self.manifest.clone(), directory: self.directory.clone() }
    }
    pub fn status(&self) -> DeliveryEntry {
        DeliveryEntry {
            range_id: self.id().into(),
            phase: self.state.phase,
            attempt: self.state.attempt.as_ref().map_or(0, |attempt| attempt.ordinal),
            retry_at_ms: self.state.retry_at_ms.to_string(),
            problem: self.state.problem,
        }
    }
}
struct Journal {
    contract: Contract,
    connection: Connection,
    _owner: durable_fs::ExclusiveFile,
}
impl Journal {
    pub fn create(root: &Path, contract: Contract) -> Result<Self> {
        contract.validate()?;
        durable_fs::create_directory(root).map_err(storage)?;
        durable_fs::create_file(&root.join("owner.lock")).map_err(storage)?.sync_all().map_err(storage)?;
        durable_fs::create_file(&root.join("delivery.sqlite3")).map_err(storage)?.sync_all().map_err(storage)?;
        let owner = own(root)?;
        let connection = Connection::open(root.join("delivery.sqlite3")).map_err(storage)?;
        configure(&connection)?;
        connection.execute_batch("CREATE TABLE identity(contract TEXT NOT NULL,poll_cursor TEXT,reconcile_after TEXT,store_id TEXT);
          CREATE TABLE delivery_manifests(id TEXT PRIMARY KEY,manifest TEXT NOT NULL,sha TEXT NOT NULL,bytes INTEGER NOT NULL);
          CREATE INDEX immutable_manifest_bytes ON delivery_manifests(bytes);
          CREATE TABLE deliveries(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL REFERENCES delivery_manifests(id) ON DELETE CASCADE,directory TEXT NOT NULL,state TEXT NOT NULL,version INTEGER NOT NULL,captured_at INTEGER NOT NULL,phase TEXT NOT NULL CHECK(phase=json_extract(state,'$.phase')));
          CREATE INDEX retired_delivery ON deliveries(phase,ordinal);
          CREATE INDEX delivery_backlog ON deliveries(phase,captured_at);").map_err(storage)?;
        connection.execute("INSERT INTO identity VALUES(?1,NULL,NULL,NULL)", [encode(&contract)?]).map_err(storage)?;
        durable_fs::sync_directory(root).map_err(storage)?;
        Ok(Self { contract, connection, _owner: owner })
    }
    pub fn open(root: &Path, contract: Contract) -> Result<Self> {
        contract.validate()?;
        let owner = own(root)?;
        durable_fs::regular_file(&root.join("delivery.sqlite3")).map_err(storage)?;
        let connection = Connection::open(root.join("delivery.sqlite3")).map_err(storage)?;
        configure(&connection)?;
        let stored: String =
            connection.query_row("SELECT contract FROM identity", [], |row| row.get(0)).map_err(storage)?;
        if decode::<Contract>(&stored)? != contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(Self { contract, connection, _owner: owner })
    }
    pub fn register(&mut self, range: &PreparedRange) -> Result<()> {
        self.insert_range(range.manifest(), &range.directory, &State::default(), HydrationPriority::Current)
    }
    fn bind_store(&self, store_id: &str) -> Result<()> {
        if !valid_digest(store_id) {
            return Err(integrity());
        }
        let existing: Option<String> =
            self.connection.query_row("SELECT store_id FROM identity", [], |row| row.get(0)).map_err(storage)?;
        if existing.as_deref().is_some_and(|existing| existing != store_id) {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        self.connection
            .execute("UPDATE identity SET store_id=?1 WHERE store_id IS NULL", [store_id])
            .map_err(storage)?;
        Ok(())
    }
    fn restore(&mut self, range: &CommittedRange, priority: HydrationPriority) -> Result<()> {
        if range.manifest().digest()? != range.manifest_digest() {
            return Err(integrity());
        }
        if range.manifest().contract != self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        self.bind_store(range.store_id())?;
        let state = State { phase: DeliveryPhase::Committed, ..State::default() };
        self.insert_range(range.manifest(), Path::new(""), &state, priority)?;
        let existing = self.load(&range.manifest().range_id)?;
        if existing.state.phase == DeliveryPhase::Captured {
            self.save(existing.id(), &state, existing.version)?;
        }
        Ok(())
    }
    fn insert_range(
        &mut self,
        manifest: &RangeManifest,
        directory: &Path,
        initial: &State,
        priority: HydrationPriority,
    ) -> Result<()> {
        manifest.validate()?;
        if manifest.contract != self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let json = encode(manifest)?;
        let sha = digest(json.as_bytes());
        let directory = directory.to_str().ok_or_else(integrity)?;
        if directory.len() > 4096 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let tx = self.connection.transaction().map_err(storage)?;
        let existing: Option<String> = tx
            .query_row("SELECT sha FROM delivery_manifests WHERE id=?1", [&manifest.range_id], |row| row.get(0))
            .optional()
            .map_err(storage)?;
        if let Some(existing) = existing {
            if existing != sha {
                return Err(integrity());
            }
            return Ok(());
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM deliveries", [], |row| row.get(0)).map_err(storage)?;
        if count >= i64::from(priority.range_limit()) {
            return Err(DeliveryProblem::Capacity.error());
        }
        let used: i64 = tx
            .query_row("SELECT coalesce(sum(bytes),0) FROM delivery_manifests", [], |row| row.get(0))
            .map_err(storage)?;
        if u64::try_from(used)
            .map_err(|_| integrity())?
            .checked_add(json.len() as u64)
            .is_none_or(|total| total > priority.metadata_limit())
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        tx.execute(
            "INSERT INTO delivery_manifests VALUES(?1,?2,?3,?4)",
            params![manifest.range_id, json, sha, json.len() as i64],
        )
        .map_err(storage)?;
        tx.execute(
            "INSERT INTO deliveries(id,directory,state,version,captured_at,phase) VALUES(?1,?2,?3,0,?4,json_extract(?3,'$.phase'))",
            params![
                manifest.range_id,
                directory,
                encode(initial)?,
                chrono::DateTime::parse_from_rfc3339(&manifest.collected_until)
                    .map_err(|_| integrity())?
                    .timestamp_millis()
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    pub(crate) fn load(&self, id: &str) -> Result<PendingRange> {
        if !valid_digest(id) {
            return Err(integrity());
        }
        let (json, sha, directory, state, version): (String, String, String, String, i64) = self
            .connection
            .query_row("SELECT m.manifest,m.sha,d.directory,d.state,d.version FROM deliveries d JOIN delivery_manifests m ON m.id=d.id WHERE d.id=?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .optional()
            .map_err(storage)?
            .ok_or_else(|| DeliveryProblem::NotReady.error())?;
        if json.len() > MAX_MANIFEST_BYTES || digest(json.as_bytes()) != sha {
            return Err(integrity());
        }
        let manifest: RangeManifest = decode(&json)?;
        manifest.validate()?;
        if manifest.contract != self.contract || manifest.range_id != id {
            return Err(integrity());
        }
        Ok(PendingRange {
            manifest: Arc::new(manifest),
            manifest_digest: sha,
            manifest_bytes: json.len(),
            directory: directory.into(),
            state: decode(&state)?,
            version,
        })
    }
    fn save(&mut self, id: &str, state: &State, version: i64) -> Result<i64> {
        let next_version = version.checked_add(1).ok_or_else(integrity)?;
        let changed = self
            .connection
            .execute(
                "UPDATE deliveries SET state=?1,phase=json_extract(?1,'$.phase'),version=version+1 WHERE id=?2 AND version=?3",
                params![encode(state)?, id, version],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(DeliveryProblem::Conflict.error());
        }
        Ok(next_version)
    }
    fn backlog(&self) -> Result<DeliveryBacklog> {
        // The covering index keeps this independent of native payload/manifest size.
        let mut statement = self.connection.prepare(BACKLOG_QUERY).map_err(storage)?;
        let mut rows = statement.query([]).map_err(storage)?;
        let mut result = DeliveryBacklog::default();
        while let Some(row) = rows.next().map_err(storage)? {
            let phase: String = row.get(0).map_err(storage)?;
            let phase: DeliveryPhase =
                serde_json::from_value(serde_json::Value::String(phase)).map_err(|_| integrity())?;
            let count: u32 = row.get(1).map_err(storage)?;
            match phase {
                DeliveryPhase::Retired => (),
                DeliveryPhase::Quarantined => result.quarantined = count,
                _ => {
                    result.pending = result.pending.checked_add(count).ok_or_else(integrity)?;
                    if let Some(at) = row.get::<_, Option<i64>>(2).map_err(storage)? {
                        result.oldest_pending_collection_ms =
                            Some(result.oldest_pending_collection_ms.map_or(at, |old| old.min(at)));
                    }
                }
            }
        }
        Ok(result)
    }
    fn status(&self, id: &str) -> Result<DeliveryEntry> {
        if !valid_digest(id) {
            return Err(integrity());
        }
        let row:Option<(String,Option<u32>,i64,Option<String>)>=self.connection.query_row(
            "SELECT phase,json_extract(state,'$.attempt.ordinal'),json_extract(state,'$.retry_at_ms'),json_extract(state,'$.problem') FROM deliveries WHERE id=?1",
            [id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional().map_err(storage)?;
        let (phase, attempt, retry, problem) = row.ok_or_else(|| DeliveryProblem::NotReady.error())?;
        let decode_enum = |text: String| serde_json::Value::String(text);
        Ok(DeliveryEntry {
            range_id: id.into(),
            phase: serde_json::from_value(decode_enum(phase)).map_err(|_| integrity())?,
            attempt: attempt.unwrap_or(0),
            retry_at_ms: retry.to_string(),
            problem: problem
                .map(|value| serde_json::from_value(decode_enum(value)).map_err(|_| integrity()))
                .transpose()?,
        })
    }
    pub fn pending(&self, after: u64, limit: u32) -> Result<(Vec<String>, Option<u64>)> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let after = i64::try_from(after).map_err(|_| integrity())?;
        let mut statement = self
            .connection
            .prepare("SELECT ordinal,id FROM deliveries WHERE ordinal>?1 AND phase NOT IN ('retired','quarantined') ORDER BY ordinal LIMIT ?2")
            .map_err(storage)?;
        let rows = statement
            .query_map(params![after, limit + 1], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(storage)?;
        let mut ids = Vec::new();
        let mut last = after;
        let mut next = None;
        for (visited, row) in rows.enumerate() {
            let (ordinal, id) = row.map_err(storage)?;
            if visited == limit as usize {
                next = Some(u64::try_from(last).map_err(|_| integrity())?);
                break;
            }
            last = ordinal;
            ids.push(id);
        }
        Ok((ids, next))
    }
    fn recent_pending(&self, limit: u32) -> Result<Vec<String>> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT id FROM deliveries WHERE phase NOT IN ('retired','quarantined') ORDER BY ordinal DESC LIMIT ?1",
            )
            .map_err(storage)?;
        statement
            .query_map([limit], |row| row.get(0))
            .map_err(storage)?
            .collect::<std::result::Result<_, _>>()
            .map_err(storage)
    }
    pub fn poll_cursor(&self) -> Result<Option<String>> {
        self.connection.query_row("SELECT poll_cursor FROM identity", [], |row| row.get(0)).map_err(storage)
    }
    pub fn apply_updates(&mut self, updates: &DeliveryUpdates) -> Result<()> {
        if updates.updates.len() > MAX_UPDATE_PAGE
            || updates.next_cursor.len() > culprit_core::telemetry::MAX_CURSOR_BYTES
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        let tx = self.connection.transaction().map_err(storage)?;
        for update in &updates.updates {
            let (id, quarantine) = match update {
                DeliveryUpdate::Retry { range_id } | DeliveryUpdate::Reconcile { range_id } => (range_id, false),
                DeliveryUpdate::Quarantine { range_id } => (range_id, true),
            };
            if !valid_digest(id) {
                return Err(integrity());
            }
            let state: Option<String> = tx
                .query_row("SELECT state FROM deliveries WHERE id=?1", [id], |row| row.get(0))
                .optional()
                .map_err(storage)?;
            if let Some(state) = state {
                let mut state: State = decode(&state)?;
                state.reconcile = true;
                state.verification = None;
                if quarantine && !matches!(state.phase, DeliveryPhase::Published | DeliveryPhase::Retired) {
                    state.phase = DeliveryPhase::Quarantined;
                    state.problem = Some(DeliveryProblem::Integrity);
                }
                tx.execute(
                    "UPDATE deliveries SET state=?1,phase=json_extract(?1,'$.phase'),version=version+1 WHERE id=?2",
                    params![encode(&state)?, id],
                )
                .map_err(storage)?;
            }
        }
        tx.execute(
            "UPDATE identity SET poll_cursor=?1,reconcile_after=CASE WHEN ?2 THEN '0' ELSE reconcile_after END",
            params![updates.next_cursor, updates.expired],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    /// Expired notification status starts a persisted, bounded scan of pending ranges.
    pub fn reconcile_expired(&mut self, limit: u32) -> Result<u32> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let tx = self.connection.transaction().map_err(storage)?;
        let after: Option<String> =
            tx.query_row("SELECT reconcile_after FROM identity", [], |row| row.get(0)).map_err(storage)?;
        let Some(after) = after else { return Ok(0) };
        let after = after.parse::<i64>().map_err(|_| integrity())?;
        let selected = {
            let mut query = tx
                .prepare("SELECT ordinal,id,state FROM deliveries WHERE ordinal>?1 ORDER BY ordinal LIMIT ?2")
                .map_err(storage)?;
            query
                .query_map(params![after, limit], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                })
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?
        };
        for (_, id, state) in &selected {
            let mut state: State = decode(state)?;
            if !matches!(state.phase, DeliveryPhase::Retired | DeliveryPhase::Quarantined) {
                state.reconcile = true;
                state.verification = None;
                tx.execute(
                    "UPDATE deliveries SET state=?1,phase=json_extract(?1,'$.phase'),version=version+1 WHERE id=?2",
                    params![encode(&state)?, id],
                )
                .map_err(storage)?;
            }
        }
        let next = if selected.len() == limit as usize { selected.last().map(|row| row.0.to_string()) } else { None };
        tx.execute("UPDATE identity SET reconcile_after=?1", [next]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(selected.len() as u32)
    }
}
/// Runtime journal I/O is serialized on an owned blocking worker. Retiring an async caller cannot release the
/// file lock or let another state transition pass a transaction whose outcome is still unknown.
pub struct DeliveryJournal {
    state: BlockingState<Journal>,
}
#[derive(Clone)]
pub struct DeliveryHealthReader {
    state: BlockingState<Journal>,
}
impl DeliveryHealthReader {
    pub async fn backlog(&self) -> Result<DeliveryBacklog> {
        self.state.run(|| |journal| journal.backlog()).await
    }
}
impl DeliveryJournal {
    pub fn health_reader(&self) -> DeliveryHealthReader {
        DeliveryHealthReader { state: self.state.clone() }
    }
    pub fn create(root: &Path, contract: Contract) -> Result<Self> {
        Ok(Self { state: BlockingState::new(Journal::create(root, contract)?, || DeliveryProblem::Storage.error()) })
    }
    pub fn open(root: &Path, contract: Contract) -> Result<Self> {
        Ok(Self { state: BlockingState::new(Journal::open(root, contract)?, || DeliveryProblem::Storage.error()) })
    }
    pub async fn register(&self, range: &PreparedRange) -> Result<()> {
        self.state
            .run(|| {
                let range = PreparedRange { manifest: range.manifest.clone(), directory: range.directory.clone() };
                move |journal| journal.register(&range)
            })
            .await
    }
    pub async fn bind_store(&self, store_id: &str) -> Result<()> {
        self.state
            .run(|| {
                let id = store_id.to_owned();
                move |journal| journal.bind_store(&id)
            })
            .await
    }
    /// Restores delivery from a verified remote capability. Original local payload files are unnecessary.
    pub async fn restore(&self, range: &CommittedRange, priority: HydrationPriority) -> Result<()> {
        self.state
            .run(|| {
                let range = range.clone();
                move |journal| journal.restore(&range, priority)
            })
            .await
    }
    pub(super) async fn forget_archive(&self, range: &CommittedRange) -> Result<()> {
        self.state
            .run(|| {
                let range = range.clone();
                move |journal| {
                    journal.bind_store(range.store_id())?;
                    let entry = journal.load(&range.manifest().range_id)?;
                    if entry.state.phase != DeliveryPhase::Committed || entry.manifest.as_ref() != range.manifest() {
                        return Err(integrity());
                    }
                    journal
                        .connection
                        .execute("DELETE FROM delivery_manifests WHERE id=?1", [entry.id()])
                        .map_err(storage)?;
                    Ok(())
                }
            })
            .await
    }
    pub(crate) async fn load(&self, id: &str) -> Result<PendingRange> {
        self.state
            .run(|| {
                let id = id.to_owned();
                move |journal| journal.load(&id)
            })
            .await
    }
    pub async fn status(&self, id: &str) -> Result<DeliveryEntry> {
        self.state
            .run(|| {
                let id = id.to_owned();
                move |journal| journal.status(&id)
            })
            .await
    }
    pub(crate) async fn compact_retired(&self, limit: u32) -> Result<()> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.state
            .run(|| {
                move |journal| {
                    journal
                        .connection
                        .execute(
                            "DELETE FROM delivery_manifests WHERE id IN (
                SELECT id FROM deliveries WHERE json_extract(state,'$.phase')=json_extract(?1,'$')
                ORDER BY ordinal LIMIT ?2)",
                            params![encode(&DeliveryPhase::Retired)?, limit],
                        )
                        .map_err(storage)?;
                    Ok(())
                }
            })
            .await
    }
    pub(crate) async fn save(&self, entry: &mut PendingRange) -> Result<()> {
        entry.version = self
            .state
            .run(|| {
                let id = entry.id().to_owned();
                let state = entry.state.clone();
                let version = entry.version;
                move |journal| journal.save(&id, &state, version)
            })
            .await?;
        Ok(())
    }
    pub async fn pending(&self, after: u64, limit: u32) -> Result<(Vec<String>, Option<u64>)> {
        self.state.run(|| move |journal| journal.pending(after, limit)).await
    }
    pub(super) async fn recent_pending(&self, limit: u32) -> Result<Vec<String>> {
        self.state.run(|| move |journal| journal.recent_pending(limit)).await
    }
    pub async fn poll_cursor(&self) -> Result<Option<String>> {
        self.state.run(|| |journal| journal.poll_cursor()).await
    }
    pub async fn apply_updates(&self, updates: DeliveryUpdates) -> Result<()> {
        self.state.run(|| move |journal| journal.apply_updates(&updates)).await
    }
    pub async fn reconcile_expired(&self, limit: u32) -> Result<u32> {
        self.state.run(|| move |journal| journal.reconcile_expired(limit)).await
    }
}
fn own(root: &Path) -> Result<durable_fs::ExclusiveFile> {
    durable_fs::own_file(&root.join("owner.lock")).map_err(|error| match error {
        durable_fs::OwnershipError::InvalidFile => integrity(),
        durable_fs::OwnershipError::Contended => DeliveryProblem::Fenced.error(),
        durable_fs::OwnershipError::Io(error) => storage(error),
    })
}

fn configure(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA trusted_schema=OFF;PRAGMA foreign_keys=ON;",
        )
        .map_err(storage)?;
    connection.pragma_update(None, "max_page_count", JOURNAL_PAGES).map_err(storage)?;
    Ok(())
}

#[cfg(test)]
mod health_tests {
    use super::*;
    #[tokio::test]
    async fn historical_hydration_leaves_capacity_for_current_observations_and_preserves_exact_identity() {
        use crate::delivery::{file_store::FileRecoveryStore, recovery::stage_and_commit};
        let root = tempfile::tempdir().unwrap();
        let contract = crate::delivery::tests::contract();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut head = None;
        let mut committed = vec![];
        for sequence in 1..=3 {
            let prepared = crate::delivery::tests::prepared(root.path(), sequence);
            let range = stage_and_commit(&store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
            head = Some(range.manifest().range_id.clone());
            committed.push(range);
        }
        let mut journal = Journal::create(&root.path().join("journal"), contract).unwrap();
        journal.restore(&committed[0], HydrationPriority::Historical).unwrap();
        // Exercise admission accounting without manufacturing hundreds of MiB of immutable payload.
        journal
            .connection
            .execute("UPDATE delivery_manifests SET bytes=?1", [HydrationPriority::Historical.metadata_limit() as i64])
            .unwrap();
        let error = journal.restore(&committed[1], HydrationPriority::Historical).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
        journal.restore(&committed[2], HydrationPriority::Current).unwrap();
        assert_eq!(
            journal.load(&committed[2].manifest().range_id).unwrap().manifest_digest,
            committed[2].manifest_digest()
        );
        assert!(journal.load(&committed[1].manifest().range_id).is_err());
        journal.connection.execute("UPDATE delivery_manifests SET bytes=length(CAST(manifest AS BLOB))", []).unwrap();
        journal.restore(&committed[1], HydrationPriority::Historical).unwrap();
        assert_eq!(journal.load(&committed[1].manifest().range_id).unwrap().state.phase, DeliveryPhase::Committed);
    }

    #[test]
    fn backlog_distinguishes_quarantine_and_retirement_without_scanning_large_manifests() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::create(&root.path().join("journal"), crate::delivery::tests::contract()).unwrap();
        let mut captures = vec![];
        for sequence in 1..=3 {
            let capture = crate::delivery::tests::prepared(root.path(), sequence);
            journal.register(&capture).unwrap();
            captures.push(capture);
        }
        assert_eq!(journal.backlog().unwrap().pending, 3);
        let mut entry = journal.load(&captures[0].manifest().range_id).unwrap();
        entry.state.phase = DeliveryPhase::Retired;
        journal.save(entry.id(), &entry.state, entry.version).unwrap();
        let mut entry = journal.load(&captures[1].manifest().range_id).unwrap();
        entry.state.phase = DeliveryPhase::Quarantined;
        journal.save(entry.id(), &entry.state, entry.version).unwrap();
        let backlog = journal.backlog().unwrap();
        assert_eq!((backlog.pending, backlog.quarantined), (1, 1));
        let expected = vec![captures[2].manifest().range_id.clone()];
        assert_eq!(
            journal.pending(0, 1).unwrap(),
            (expected.clone(), None),
            "finished work cannot hide the first live range"
        );
        assert_eq!(journal.recent_pending(1).unwrap(), expected);
        assert_eq!(
            backlog.oldest_pending_collection_ms,
            Some(
                chrono::DateTime::parse_from_rfc3339(&captures[2].manifest().collected_until)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        let mut statement = journal.connection.prepare(&format!("EXPLAIN QUERY PLAN {BACKLOG_QUERY}")).unwrap();
        let plan = statement
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("COVERING INDEX delivery_backlog"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}
