//! Persistent local analytical test destination. Network, cloud ingestion and cloud capacity need separate validation.
use super::{
    contract::*,
    evidence::*,
    recovery::{CommittedRange, RecoveryStore},
    sink::*,
};
use crate::{blocking_state::BlockingState, durable_fs};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};
fn storage(_: impl std::fmt::Display) -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}
fn integrity() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

pub struct FileTelemetrySink {
    contract: Contract,
    recovery: Arc<dyn RecoveryStore>,
    warehouse: BlockingState<Warehouse>,
}
impl FileTelemetrySink {
    pub fn create(root: &Path, contract: Contract, recovery: Arc<dyn RecoveryStore>) -> Result<Self> {
        contract.validate()?;
        durable_fs::create_directory(root).map_err(storage)?;
        durable_fs::create_file(&root.join("warehouse.sqlite3")).map_err(storage)?.sync_all().map_err(storage)?;
        let journal = Connection::open(root.join("warehouse.sqlite3")).map_err(storage)?;
        configure(&journal)?;
        journal
            .execute_batch(
                "CREATE TABLE identity(contract TEXT NOT NULL);
          CREATE TABLE submissions(attempt TEXT NOT NULL,batch TEXT NOT NULL,range_id TEXT NOT NULL,PRIMARY KEY(attempt,batch));
          CREATE TABLE landing(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,range_id TEXT NOT NULL,receipt TEXT NOT NULL);
          CREATE TABLE publications(range_id TEXT PRIMARY KEY,manifest_sha TEXT NOT NULL,receipts_sha TEXT NOT NULL);
          CREATE INDEX landing_range ON landing(range_id,ordinal);",
            )
            .map_err(storage)?;
        journal
            .execute("INSERT INTO identity VALUES(?1)", [serde_json::to_string(&contract).map_err(storage)?])
            .map_err(storage)?;
        durable_fs::sync_directory(root).map_err(storage)?;
        Ok(Self {
            contract,
            recovery,
            warehouse: BlockingState::new(Warehouse { journal }, || DeliveryProblem::Storage.error()),
        })
    }
    pub fn open(root: &Path, contract: Contract, recovery: Arc<dyn RecoveryStore>) -> Result<Self> {
        contract.validate()?;
        durable_fs::regular_file(&root.join("warehouse.sqlite3")).map_err(storage)?;
        let journal = Connection::open(root.join("warehouse.sqlite3")).map_err(storage)?;
        configure(&journal)?;
        let value: String =
            journal.query_row("SELECT contract FROM identity", [], |row| row.get(0)).map_err(storage)?;
        if serde_json::from_str::<Contract>(&value).map_err(|_| integrity())? != contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(Self {
            contract,
            recovery,
            warehouse: BlockingState::new(Warehouse { journal }, || DeliveryProblem::Storage.error()),
        })
    }
    pub async fn physical_rows(&self, range_id: &str) -> Result<u64> {
        self.warehouse
            .run(|| {
                let id = range_id.to_owned();
                move |warehouse: &mut Warehouse| warehouse.physical_rows(&id)
            })
            .await
    }
}
fn configure(journal: &Connection) -> Result<()> {
    journal
        .execute_batch(
            "PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA trusted_schema=OFF;PRAGMA max_page_count=524288;",
        )
        .map_err(storage)?;
    Ok(())
}
#[async_trait]
impl TelemetrySink for FileTelemetrySink {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        if contract != &self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(())
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.validate_contract(&range.manifest().contract).await?;
        if range.store_id() != self.recovery.id() || !bounded_id(&attempt.id) || attempt.ordinal == 0 {
            return Err(integrity());
        }
        let access = self.warehouse.acquire().await;
        let mut receipts = Vec::new();
        let batch = range
            .manifest()
            .batches
            .iter()
            .find(|batch| batch.batch_id == batch_id && batch.delivery_required)
            .ok_or_else(integrity)?;
        let mut records = super::recovery::RecoveryRecords::open(self.recovery.as_ref(), batch).await?;
        while let Some(row) = records.next().await? {
            if row.capture != range.manifest().capture || row.batch_id != batch.batch_id {
                return Err(integrity());
            }
            receipts.push(RowReceipt::from_export(&row)?);
            if receipts.len() > MAX_RANGE_RECORDS {
                return Err(DeliveryProblem::Capacity.error());
            }
        }
        if receipts != batch.rows {
            return Err(integrity());
        }
        let range = range.clone();
        let attempt = attempt.clone();
        let batch_id = batch_id.to_owned();
        access.run(move |warehouse: &mut Warehouse| warehouse.dispatch(&range, &attempt, &batch_id, receipts)).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        self.warehouse
            .run(|| {
                let range = range.clone();
                let cursor = cursor.map(str::to_owned);
                move |warehouse: &mut Warehouse| warehouse.receipts(&range, cursor.as_deref(), page_rows)
            })
            .await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.validate_contract(&range.committed().manifest().contract).await?;
        self.warehouse
            .run(|| {
                let range = range.clone();
                let attempt = attempt.clone();
                move |warehouse: &mut Warehouse| warehouse.publish(&range, &attempt)
            })
            .await
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.warehouse
            .run(|| {
                let publication = publication.clone();
                let range = range.clone();
                move |warehouse: &mut Warehouse| warehouse.publication_visible(&publication, &range)
            })
            .await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(DeliveryUpdates { updates: vec![], next_cursor: cursor.unwrap_or("0").into(), expired: false })
    }
}

struct Warehouse {
    journal: Connection,
}
impl Warehouse {
    fn physical_rows(&self, range_id: &str) -> Result<u64> {
        let count: i64 = self
            .journal
            .query_row("SELECT count(*) FROM landing WHERE range_id=?1", [range_id], |row| row.get(0))
            .map_err(storage)?;
        count.try_into().map_err(|_| integrity())
    }
    fn dispatch(
        &mut self,
        range: &CommittedRange,
        attempt: &DeliveryAttempt,
        batch_id: &str,
        receipts: Vec<RowReceipt>,
    ) -> Result<Submission> {
        let journal = &mut self.journal;
        let tx = journal.transaction().map_err(storage)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT range_id FROM submissions WHERE attempt=?1 AND batch=?2",
                params![attempt.id, batch_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(existing) = existing {
            if existing != range.manifest().range_id {
                return Err(integrity());
            }
            return Ok(Submission::Accepted);
        }
        for receipt in receipts {
            tx.execute(
                "INSERT INTO landing(range_id,receipt) VALUES(?1,?2)",
                params![range.manifest().range_id, serde_json::to_string(&receipt).map_err(storage)?],
            )
            .map_err(storage)?;
        }
        tx.execute(
            "INSERT INTO submissions VALUES(?1,?2,?3)",
            params![attempt.id, batch_id, range.manifest().range_id],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Submission::Accepted)
    }
    fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        if page_rows == 0 || page_rows > 10_000 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let after = cursor.map(|value| value.parse::<i64>().map_err(|_| integrity())).transpose()?.unwrap_or(0);
        if after < 0 {
            return Err(integrity());
        }
        let journal = &self.journal;
        let mut statement = journal
            .prepare("SELECT ordinal,receipt FROM landing WHERE range_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")
            .map_err(storage)?;
        let mut selected =
            statement.query(params![range.manifest().range_id, after, page_rows + 1]).map_err(storage)?;
        let mut rows = Vec::new();
        let mut last = after;
        let mut next = None;
        while let Some(row) = selected.next().map_err(storage)? {
            if rows.len() == page_rows as usize {
                next = Some(last.to_string());
                break;
            }
            last = row.get(0).map_err(storage)?;
            let json: String = row.get(1).map_err(storage)?;
            rows.push(serde_json::from_str(&json).map_err(|_| integrity())?);
        }
        Ok(ReceiptPage { rows, next_cursor: next, complete: true })
    }
    fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        let committed = range.committed();
        if !bounded_id(&attempt.id) || attempt.ordinal == 0 {
            return Err(integrity());
        }
        let journal = &self.journal;
        journal
            .execute(
                "INSERT OR IGNORE INTO publications VALUES(?1,?2,?3)",
                params![committed.manifest().range_id, committed.manifest_digest(), range.receipts_digest()],
            )
            .map_err(storage)?;
        let (sha, receipts): (String, String) = journal
            .query_row(
                "SELECT manifest_sha,receipts_sha FROM publications WHERE range_id=?1",
                [&committed.manifest().range_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(storage)?;
        if sha != committed.manifest_digest() || receipts != range.receipts_digest() {
            return Err(integrity());
        }
        Ok(Publication { range_id: committed.manifest().range_id.clone(), manifest_digest: sha })
    }
    fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        if publication.range_id != range.manifest().range_id || publication.manifest_digest != range.manifest_digest() {
            return Err(integrity());
        }
        let sha: Option<String> = self
            .journal
            .query_row("SELECT manifest_sha FROM publications WHERE range_id=?1", [&publication.range_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(storage)?;
        match sha {
            Some(sha) if sha != publication.manifest_digest => Err(integrity()),
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
}
