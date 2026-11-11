//! Local protocol destination for executable recovery tests. It does not establish cloud durability guarantees.
use super::{contract::*, evidence::*, recovery::*};
use crate::{blocking_state::BlockingState, durable_fs};
use async_trait::async_trait;
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const RECOVERY_PAGE_LIMIT: u32 = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const JOURNAL_PAGE_LIMIT: u32 = 524_288;
fn storage(_: impl std::fmt::Display) -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}
fn integrity() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

struct RecoveryFiles {
    root: PathBuf,
    id: String,
    contract: Contract,
    journal: Connection,
}
pub struct FileOwnership {
    fence: Fence,
    _lock: durable_fs::ExclusiveFile,
}
impl FileOwnership {
    pub fn fence(&self) -> &Fence {
        &self.fence
    }
}
impl RecoveryFiles {
    pub fn create(root: &Path, contract: Contract) -> Result<Self> {
        contract.validate()?;
        durable_fs::create_directory(root).map_err(storage)?;
        durable_fs::create_directory(&root.join("payloads")).map_err(storage)?;
        durable_fs::create_file(&root.join("owner.lock")).map_err(storage)?.sync_all().map_err(storage)?;
        durable_fs::create_file(&root.join("recovery.sqlite3")).map_err(storage)?.sync_all().map_err(storage)?;
        let journal = Connection::open(root.join("recovery.sqlite3")).map_err(storage)?;
        configure(&journal)?;
        journal.execute_batch("CREATE TABLE identity(id TEXT PRIMARY KEY,contract TEXT NOT NULL,epoch INTEGER NOT NULL,token TEXT NOT NULL,head TEXT,high_sequence INTEGER NOT NULL,activity_heads TEXT NOT NULL,pending TEXT NOT NULL);
          CREATE TABLE objects(object TEXT PRIMARY KEY,sha TEXT NOT NULL,bytes INTEGER NOT NULL);
          CREATE TABLE ranges(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,previous TEXT,sha TEXT NOT NULL,manifest TEXT NOT NULL);
          CREATE TABLE checkpoints(stream_key TEXT PRIMARY KEY,cursor TEXT NOT NULL);
          CREATE TABLE coverage(range_key TEXT PRIMARY KEY,range_id TEXT NOT NULL REFERENCES ranges(id));").map_err(storage)?;
        let id = digest(&serde_json::to_vec(&(contract.id()?, durable_fs::random_id()?)).map_err(storage)?);
        journal
            .execute(
                "INSERT INTO identity VALUES(?1,?2,0,'',NULL,0,'{}','{}')",
                params![id, serde_json::to_string(&contract).map_err(storage)?],
            )
            .map_err(storage)?;
        durable_fs::sync_directory(root).map_err(storage)?;
        Ok(Self { root: root.into(), id, contract, journal })
    }
    pub fn open(root: &Path, contract: Contract) -> Result<Self> {
        contract.validate()?;
        durable_fs::regular_file(&root.join("recovery.sqlite3")).map_err(storage)?;
        let journal = Connection::open(root.join("recovery.sqlite3")).map_err(storage)?;
        configure(&journal)?;
        let (id, stored): (String, String) = journal
            .query_row("SELECT id,contract FROM identity", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?;
        if serde_json::from_str::<Contract>(&stored).map_err(|_| integrity())? != contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(Self { root: root.into(), id, contract, journal })
    }
    pub fn acquire(&mut self) -> Result<FileOwnership> {
        let lock = durable_fs::own_file(&self.root.join("owner.lock")).map_err(|error| match error {
            durable_fs::OwnershipError::InvalidFile => integrity(),
            durable_fs::OwnershipError::Contended => DeliveryProblem::Fenced.error(),
            durable_fs::OwnershipError::Io(error) => storage(error),
        })?;
        let token = durable_fs::random_id()?;
        let journal = &mut self.journal;
        let transaction =
            journal.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage)?;
        let epoch: i64 = transaction.query_row("SELECT epoch FROM identity", [], |row| row.get(0)).map_err(storage)?;
        let epoch = epoch.checked_add(1).filter(|epoch| *epoch > 0).ok_or_else(integrity)?;
        transaction.execute("UPDATE identity SET epoch=?1,token=?2", params![epoch, token]).map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(FileOwnership { fence: Fence::issued(epoch as u64, token, self.id.clone())?, _lock: lock })
    }
    pub fn head(&self) -> Result<Option<String>> {
        self.journal.query_row("SELECT head FROM identity", [], |row| row.get(0)).map_err(storage)
    }
    pub fn checkpoint(&self, key: &str) -> Result<Option<String>> {
        self.journal
            .query_row("SELECT cursor FROM checkpoints WHERE stream_key=?1", [key], |row| row.get(0))
            .optional()
            .map_err(storage)
    }
    fn read_manifest(json: String, sha: String) -> Result<RangeManifest> {
        if json.len() > MAX_MANIFEST_BYTES || digest(json.as_bytes()) != sha {
            return Err(integrity());
        }
        let manifest: RangeManifest = serde_json::from_str(&json).map_err(|_| integrity())?;
        manifest.validate()?;
        Ok(manifest)
    }
}
fn configure(journal: &Connection) -> Result<()> {
    journal
        .execute_batch(
            "PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF;",
        )
        .map_err(storage)?;
    journal.pragma_update(None, "max_page_count", JOURNAL_PAGE_LIMIT).map_err(storage)?;
    Ok(())
}
fn payload_path(root: &Path, batch: &PayloadObject) -> Result<PathBuf> {
    if !valid_digest(&batch.sha256)
        || batch.object != format!("payloads/{}.ndjson", batch.sha256)
        || batch.bytes > MAX_RANGE_BYTES
    {
        return Err(integrity());
    }
    Ok(root.join(&batch.object))
}
fn verify_bytes(path: &Path, batch: &PayloadObject) -> Result<()> {
    let mut input = durable_fs::regular_file(path).map_err(storage)?.take(batch.bytes + 1);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; COPY_BUFFER_BYTES];
    loop {
        let count = input.read(&mut buffer).map_err(storage)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        hash.update(&buffer[..count]);
    }
    let actual = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if total != batch.bytes || actual != batch.sha256 {
        return Err(integrity());
    }
    Ok(())
}
impl RecoveryFiles {
    fn stage(&self, batch: &PayloadObject, path: &Path) -> Result<StagedBatch> {
        let destination = payload_path(&self.root, batch)?;
        if destination.try_exists().map_err(storage)? {
            verify_bytes(&destination, batch)?;
        } else {
            let temporary = self.root.join("payloads").join(format!(".{}.part", durable_fs::random_id()?));
            let result = (|| {
                let mut source = durable_fs::regular_file(path).map_err(storage)?.take(batch.bytes + 1);
                let mut output = durable_fs::create_file(&temporary).map_err(storage)?;
                let copied = std::io::copy(&mut source, &mut output).map_err(storage)?;
                if copied != batch.bytes {
                    return Err(integrity());
                }
                output.flush().map_err(storage)?;
                output.sync_all().map_err(storage)?;
                drop(output);
                verify_bytes(&temporary, batch)?;
                match fs::hard_link(&temporary, &destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        verify_bytes(&destination, batch)?
                    }
                    Err(error) => return Err(storage(error)),
                }
                durable_fs::sync_directory(&self.root.join("payloads")).map_err(storage)?;
                Ok(())
            })();
            let cleanup = fs::remove_file(&temporary);
            result?;
            cleanup.map_err(storage)?;
        }
        let journal = &self.journal;
        journal
            .execute(
                "INSERT OR IGNORE INTO objects VALUES(?1,?2,?3)",
                params![batch.object, batch.sha256, i64::try_from(batch.bytes).map_err(|_| integrity())?],
            )
            .map_err(storage)?;
        let (sha, bytes): (String, i64) = journal
            .query_row("SELECT sha,bytes FROM objects WHERE object=?1", [&batch.object], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(storage)?;
        if sha != batch.sha256 || u64::try_from(bytes).map_err(|_| integrity())? != batch.bytes {
            return Err(integrity());
        }
        Ok(StagedBatch {
            store_id: self.id.clone(),
            object: batch.object.clone(),
            sha256: batch.sha256.clone(),
            bytes: batch.bytes,
        })
    }
    fn commit(
        &mut self,
        fence: &Fence,
        expected_head: Option<&str>,
        manifest: &RangeManifest,
        staged: &[StagedBatch],
    ) -> Result<CommitReceipt> {
        manifest.validate()?;
        if manifest.contract != self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if staged.len() != manifest.batches.len() {
            return Err(integrity());
        }
        let json = serde_json::to_string(manifest).map_err(storage)?;
        let sha = digest(json.as_bytes());
        let journal = &mut self.journal;
        let tx = journal.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage)?;
        let (epoch, token, head): (i64, String, Option<String>) = tx
            .query_row("SELECT epoch,token,head FROM identity", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(storage)?;
        if fence.store_id() != self.id
            || i64::try_from(fence.epoch()).map_err(|_| integrity())? != epoch
            || fence.token() != token
        {
            return Err(DeliveryProblem::Fenced.error());
        }
        let existing: Option<String> = tx
            .query_row("SELECT sha FROM ranges WHERE id=?1", [&manifest.range_id], |row| row.get(0))
            .optional()
            .map_err(storage)?;
        if let Some(existing) = existing {
            if existing != sha {
                return Err(integrity());
            }
            return Ok(CommitReceipt {
                store_id: self.id.clone(),
                manifest_digest: sha,
                head: manifest.range_id.clone(),
            });
        }
        if head.as_deref() != expected_head {
            return Err(DeliveryProblem::Conflict.error());
        }
        for (batch, receipt) in manifest.batches.iter().zip(staged) {
            let object = PayloadObject::from(batch);
            let path = payload_path(&self.root, &object)?;
            verify_bytes(&path, &object)?;
            let object: Option<(String, i64)> = tx
                .query_row("SELECT sha,bytes FROM objects WHERE object=?1", [&batch.object], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
                .map_err(storage)?;
            if receipt.store_id != self.id
                || receipt.object != batch.object
                || receipt.sha256 != batch.sha256
                || receipt.bytes != batch.bytes
                || object != Some((batch.sha256.clone(), i64::try_from(batch.bytes).map_err(|_| integrity())?))
            {
                return Err(integrity());
            }
        }
        for advance in &manifest.advances {
            let previous: Option<String> = tx
                .query_row("SELECT cursor FROM checkpoints WHERE stream_key=?1", [&advance.stream_key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage)?;
            if previous != advance.previous {
                return Err(DeliveryProblem::Conflict.error());
            }
            tx.execute(
                "INSERT INTO checkpoints VALUES(?1,?2) ON CONFLICT(stream_key) DO UPDATE SET cursor=excluded.cursor",
                params![advance.stream_key, advance.next],
            )
            .map_err(storage)?;
        }
        let sequence = manifest
            .capture
            .revision
            .as_ref()
            .ok_or_else(integrity)?
            .sequence
            .parse::<i64>()
            .map_err(|_| integrity())?;
        tx.execute(
            "INSERT INTO ranges(id,previous,sha,manifest) VALUES(?1,?2,?3,?4)",
            params![manifest.range_id, head, sha, json],
        )
        .map_err(storage)?;
        let commit_order = u64::try_from(tx.last_insert_rowid()).map_err(|_| integrity())?;
        let captured = manifest.source.captured_range()?;
        if let Some(key) = coverage_key(&captured)? {
            tx.execute("INSERT OR IGNORE INTO coverage VALUES(?1,?2)", params![key, manifest.range_id])
                .map_err(storage)?;
        }
        let heads: String =
            tx.query_row("SELECT activity_heads FROM identity", [], |row| row.get(0)).map_err(storage)?;
        let mut heads: super::activity_heads::ActivityHeads = serde_json::from_str(&heads).map_err(|_| integrity())?;
        heads.validate()?;
        heads.observe(manifest, &sha)?;
        let pending: String = tx.query_row("SELECT pending FROM identity", [], |r| r.get(0)).map_err(storage)?;
        let mut pending: super::pending::PendingRanges = serde_json::from_str(&pending).map_err(|_| integrity())?;
        pending.validate()?;
        pending.insert(commit_order, &manifest.range_id)?;
        tx.execute(
            "UPDATE identity SET head=?1,high_sequence=MAX(high_sequence,?2),activity_heads=?3,pending=?4",
            params![
                manifest.range_id,
                sequence,
                serde_json::to_string(&heads).map_err(storage)?,
                serde_json::to_string(&pending).map_err(storage)?
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(CommitReceipt { store_id: self.id.clone(), manifest_digest: sha, head: manifest.range_id.clone() })
    }
    fn load(&self, range_id: &str) -> Result<RangeManifest> {
        if !valid_digest(range_id) {
            return Err(integrity());
        }
        let (json, sha): (String, String) = self
            .journal
            .query_row("SELECT manifest,sha FROM ranges WHERE id=?1", [range_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(storage)?
            .ok_or_else(|| DeliveryProblem::NotReady.error())?;
        let manifest = Self::read_manifest(json, sha)?;
        if manifest.contract != self.contract || manifest.range_id != range_id {
            return Err(integrity());
        }
        Ok(manifest)
    }
    fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        if limit == 0 || limit > RECOVERY_PAGE_LIMIT {
            return Err(DeliveryProblem::Capacity.error());
        }
        let after = cursor.map(|cursor| cursor.parse::<i64>().map_err(|_| integrity())).transpose()?.unwrap_or(0);
        if after < 0 {
            return Err(integrity());
        }
        let journal = &self.journal;
        let mut statement = journal
            .prepare("SELECT ordinal,manifest,sha FROM ranges WHERE ordinal>?1 ORDER BY ordinal LIMIT ?2")
            .map_err(storage)?;
        let mut rows = statement.query(params![after, limit + 1]).map_err(storage)?;
        let mut ranges = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        let mut last = after;
        while let Some(row) = rows.next().map_err(storage)? {
            let json: String = row.get(1).map_err(storage)?;
            if ranges.len() == limit as usize || bytes + json.len() > MAX_MANIFEST_BYTES {
                next = Some(last.to_string());
                break;
            }
            bytes += json.len();
            last = row.get(0).map_err(storage)?;
            let manifest = Self::read_manifest(json, row.get(2).map_err(storage)?)?;
            if manifest.contract != self.contract {
                return Err(integrity());
            }
            ranges.push(manifest);
        }
        let high_sequence: i64 =
            journal.query_row("SELECT high_sequence FROM identity", [], |row| row.get(0)).map_err(storage)?;
        let high_sequence = u64::try_from(high_sequence).map_err(|_| integrity())?;
        Ok(RecoveryPage { ranges, next_cursor: next, high_sequence })
    }
    fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        let path = payload_path(&self.root, batch)?;
        let file = durable_fs::regular_file(&path).map_err(storage)?;
        Ok(Box::pin(tokio::fs::File::from_std(file)))
    }
}

/// File-backed protocol adapter; blocking filesystem and SQLite work run outside the async executor.
pub struct FileRecoveryStore {
    id: String,
    files: BlockingState<RecoveryFiles>,
}
impl FileRecoveryStore {
    pub fn create(root: &Path, contract: Contract) -> Result<Self> {
        Ok(Self::from_files(RecoveryFiles::create(root, contract)?))
    }
    pub fn open(root: &Path, contract: Contract) -> Result<Self> {
        Ok(Self::from_files(RecoveryFiles::open(root, contract)?))
    }
    fn from_files(files: RecoveryFiles) -> Self {
        Self { id: files.id.clone(), files: BlockingState::new(files, || DeliveryProblem::Storage.error()) }
    }
    pub async fn acquire(&self) -> Result<FileOwnership> {
        self.files.run(|| |files: &mut RecoveryFiles| files.acquire()).await
    }
    pub async fn checkpoint(&self, key: &str) -> Result<Option<String>> {
        self.files
            .run(|| {
                let key = key.to_owned();
                move |files: &mut RecoveryFiles| files.checkpoint(&key)
            })
            .await
    }
}
#[async_trait]
impl RecoveryStore for FileRecoveryStore {
    fn id(&self) -> &str {
        &self.id
    }
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32, order: RecoveryOrder) -> Result<RecoveryPage> {
        if !(1..=128).contains(&limit) {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.files
            .run(|| {
                let cursor = cursor.map(str::to_owned);
                move |files| {
                    let (pending, high): (String, i64) = files
                        .journal
                        .query_row("SELECT pending,high_sequence FROM identity", [], |r| Ok((r.get(0)?, r.get(1)?)))
                        .map_err(storage)?;
                    let pending: super::pending::PendingRanges =
                        serde_json::from_str(&pending).map_err(|_| integrity())?;
                    // A native manifest may occupy the full 32 MiB page budget by itself.
                    let committed_order: i64 = files
                        .journal
                        .query_row("SELECT coalesce(max(ordinal),0) FROM ranges", [], |row| row.get(0))
                        .map_err(storage)?;
                    let (ids, next_cursor) = pending.page(
                        cursor.as_deref(),
                        1,
                        &files.id,
                        u64::try_from(committed_order).map_err(|_| integrity())?,
                        order,
                    )?;
                    let ranges = ids.iter().map(|id| files.load(id)).collect::<Result<_>>()?;
                    Ok(RecoveryPage {
                        ranges,
                        next_cursor,
                        high_sequence: u64::try_from(high).map_err(|_| integrity())?,
                    })
                }
            })
            .await
    }
    async fn retire(&self, fence: &Fence, publication: &super::sink::PublishedRange) -> Result<()> {
        let range = publication.committed();
        if range.store_id() != self.id {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        self.files
            .run(|| {
                let fence = fence.clone();
                let range = range.clone();
                move |files| {
                    let tx = files.journal.transaction().map_err(storage)?;
                    let (epoch, token, pending): (i64, String, String) = tx
                        .query_row("SELECT epoch,token,pending FROM identity", [], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                        })
                        .map_err(storage)?;
                    if u64::try_from(epoch).map_err(|_| integrity())? != fence.epoch()
                        || token != fence.token()
                        || fence.store_id() != files.id
                    {
                        return Err(DeliveryProblem::Fenced.error());
                    }
                    let sha: String = tx
                        .query_row("SELECT sha FROM ranges WHERE id=?1", [&range.manifest().range_id], |r| r.get(0))
                        .map_err(storage)?;
                    if sha != range.manifest_digest() {
                        return Err(integrity());
                    }
                    let mut pending: super::pending::PendingRanges =
                        serde_json::from_str(&pending).map_err(|_| integrity())?;
                    pending.validate()?;
                    pending.retire(&range.manifest().range_id);
                    tx.execute("UPDATE identity SET pending=?1", [serde_json::to_string(&pending).map_err(storage)?])
                        .map_err(storage)?;
                    tx.commit().map_err(storage)
                }
            })
            .await
    }
    async fn activity_heads(&self, cursor: Option<&str>, limit: u32) -> Result<ActivityHeadPage> {
        self.files
            .run(|| {
                let cursor = cursor.map(str::to_owned);
                move |files: &mut RecoveryFiles| {
                    let (heads, high): (String, i64) = files
                        .journal
                        .query_row("SELECT activity_heads,high_sequence FROM identity", [], |row| {
                            Ok((row.get(0)?, row.get(1)?))
                        })
                        .map_err(storage)?;
                    let heads: super::activity_heads::ActivityHeads =
                        serde_json::from_str(&heads).map_err(|_| integrity())?;
                    heads.page(cursor.as_deref(), limit, u64::try_from(high).map_err(|_| integrity())?)
                }
            })
            .await
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        self.files.run(|| |files: &mut RecoveryFiles| files.head()).await
    }
    async fn captured_range(
        &self,
        range: &culprit_core::telemetry::CapturedRange,
    ) -> Result<Option<culprit_core::telemetry::CaptureIdentity>> {
        let Some(key) = coverage_key(range)? else {
            return Ok(None);
        };
        self.files
            .run(|| {
                move |files| {
                    let id: Option<String> = files
                        .journal
                        .query_row("SELECT range_id FROM coverage WHERE range_key=?1", [&key], |row| row.get(0))
                        .optional()
                        .map_err(storage)?;
                    let Some(id) = id else {
                        return Ok(None);
                    };
                    let manifest = files.load(&id)?;
                    if coverage_key(&manifest.source.captured_range()?)?.as_deref() != Some(key.as_str()) {
                        return Err(integrity());
                    }
                    Ok(Some(manifest.capture))
                }
            })
            .await
    }
    async fn stage(&self, batch: &PayloadObject, path: &Path) -> Result<StagedBatch> {
        self.files
            .run(|| {
                let batch = batch.clone();
                let path = path.to_owned();
                move |files: &mut RecoveryFiles| files.stage(&batch, &path)
            })
            .await
    }
    async fn commit(
        &self,
        fence: &Fence,
        expected_head: Option<&str>,
        manifest: &RangeManifest,
        staged: &[StagedBatch],
    ) -> Result<CommitReceipt> {
        self.files
            .run(|| {
                let fence = fence.clone();
                let head = expected_head.map(str::to_owned);
                let manifest = manifest.clone();
                let staged = staged.to_vec();
                move |files: &mut RecoveryFiles| files.commit(&fence, head.as_deref(), &manifest, &staged)
            })
            .await
    }
    async fn load(&self, range_id: &str) -> Result<RangeManifest> {
        self.files
            .run(|| {
                let id = range_id.to_owned();
                move |files: &mut RecoveryFiles| files.load(&id)
            })
            .await
    }
    async fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.files
            .run(|| {
                let cursor = cursor.map(str::to_owned);
                move |files: &mut RecoveryFiles| files.recover(cursor.as_deref(), limit)
            })
            .await
    }
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        self.files
            .run(|| {
                let batch = batch.clone();
                move |files: &mut RecoveryFiles| files.open_object(&batch)
            })
            .await
    }
}
