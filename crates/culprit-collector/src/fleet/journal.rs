//! One static owner and bounded durable job checkpoints. Blocking writes retain ownership if their caller retires.
use super::{scheduler::state_failed, work::Checkpoint};
use crate::{blocking_state::BlockingState, durable_fs};
use culprit_core::{Result, telemetry::*};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
mod investigations;
mod scheduler;
#[cfg(test)]
pub(in crate::fleet) use investigations::FocusedSettlement;
pub(in crate::fleet) use investigations::FocusedStage;

const MAX_CHECKPOINT_BYTES: usize = 256 * 1024;
const JOURNAL_VERSION: i64 = 3;

pub struct FleetJournal {
    files: BlockingState<Files>,
    assignment: String,
}
struct Files {
    connection: Connection,
    _owner: durable_fs::ExclusiveFile,
}
impl FleetJournal {
    pub async fn open(directory: &Path, inventory: &FleetInventory, shard: &str) -> Result<Self> {
        inventory.validate(shard)?;
        let assignment = digest(&serde_json::to_vec(&(shard, inventory)).map_err(|_| state_failed())?);
        let directory = directory.to_owned();
        let stored_assignment = assignment.clone();
        let files = tokio::task::spawn_blocking(move || Files::open(&directory, &stored_assignment))
            .await
            .map_err(|_| state_failed())??;
        Ok(Self { files: BlockingState::new(files, state_failed), assignment })
    }
    pub(super) fn validate_assignment(&self, inventory: &FleetInventory, shard: &str) -> Result<()> {
        let wanted = digest(&serde_json::to_vec(&(shard, inventory)).map_err(|_| state_failed())?);
        if wanted != self.assignment {
            return Err(culprit_core::Error::invalid(culprit_core::t!("fleet.assignment_changed")));
        }
        Ok(())
    }
    pub(super) async fn load(&self, key: &str) -> Result<Checkpoint> {
        self.files
            .run(|| {
                let key = key.to_owned();
                move |files| files.load(&key)
            })
            .await
    }
    pub(super) async fn save(&self, key: &str, checkpoint: &Checkpoint) -> Result<()> {
        self.save_progress(key, checkpoint, None).await
    }
    pub(super) async fn save_progress(&self, key: &str, checkpoint: &Checkpoint, gap: Option<FleetGap>) -> Result<()> {
        let bytes = serde_json::to_string(checkpoint).map_err(|_| state_failed())?;
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(state_failed());
        }
        self.files
            .run(|| {
                let key = key.to_owned();
                move |files| files.save(&key, &bytes, gap)
            })
            .await
    }
    /// Unresolved coverage gaps remain durable independently of stdout and destination availability.
    pub async fn gaps(&self, after: Option<&str>, limit: u32) -> Result<FleetGapPage> {
        if !(1..=500).contains(&limit) {
            return Err(state_failed());
        }
        let after = after.unwrap_or("0").parse::<i64>().map_err(|_| state_failed())?;
        if after < 0 {
            return Err(state_failed());
        }
        self.files.run(|| move |files| files.gaps(after, limit)).await
    }
}
impl Files {
    fn open(directory: &Path, assignment: &str) -> Result<Self> {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(state_failed()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                durable_fs::create_directory(directory).map_err(|_| state_failed())?;
            }
            Err(_) => return Err(state_failed()),
        }
        let owner_path = directory.join("owner.lock");
        match durable_fs::create_file(&owner_path) {
            Ok(file) => file.sync_all().map_err(|_| state_failed())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(state_failed()),
        }
        let owner = durable_fs::own_file(&owner_path).map_err(|_| state_failed())?;
        let path = directory.join("schedule.sqlite3");
        let created = match durable_fs::create_file(&path) {
            Ok(file) => {
                file.sync_all().map_err(|_| state_failed())?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                durable_fs::regular_file(&path).map_err(|_| state_failed())?;
                false
            }
            Err(_) => return Err(state_failed()),
        };
        let connection = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| state_failed())?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA mmap_size=0; PRAGMA max_page_count=16384; PRAGMA journal_size_limit=1048576;")
            .map_err(|_| state_failed())?;
        if created {
            let transaction = connection.unchecked_transaction().map_err(|_| state_failed())?;
            transaction.execute_batch("PRAGMA user_version=3; CREATE TABLE assignment(id TEXT NOT NULL); CREATE TABLE jobs(id TEXT PRIMARY KEY,state_json TEXT NOT NULL); CREATE TABLE gaps(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,event_json TEXT NOT NULL);
                CREATE TABLE admission_clock(id INTEGER PRIMARY KEY CHECK(id=1),state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE admission_jobs(id TEXT PRIMARY KEY,state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE admission_resources(id TEXT PRIMARY KEY,state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE admission_families(id TEXT PRIMARY KEY,state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE admission_focused(id TEXT PRIMARY KEY,state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE admission_focus_families(id TEXT PRIMARY KEY,state_json TEXT NOT NULL CHECK(length(state_json)<=4096));
                CREATE TABLE investigation_intents(id TEXT PRIMARY KEY,episode_id TEXT UNIQUE NOT NULL,stage_key TEXT UNIQUE NOT NULL,intent_json TEXT NOT NULL CHECK(length(intent_json)<=262144),intent_sha TEXT NOT NULL);
                CREATE TABLE investigation_receipts(id TEXT PRIMARY KEY,episode_id TEXT UNIQUE NOT NULL,stage_key TEXT UNIQUE NOT NULL,receipt_json TEXT NOT NULL CHECK(length(receipt_json)<=4096));")
                .map_err(|_| state_failed())?;
            transaction.execute("INSERT INTO assignment VALUES(?1)", [assignment]).map_err(|_| state_failed())?;
            transaction.commit().map_err(|_| state_failed())?;
            durable_fs::sync_directory(directory).map_err(|_| state_failed())?;
        }
        let version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(|_| state_failed())?;
        let stored: Vec<String> = connection
            .prepare("SELECT id FROM assignment")
            .map_err(|_| state_failed())?
            .query_map([], |row| row.get(0))
            .map_err(|_| state_failed())?
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| state_failed())?;
        if version != JOURNAL_VERSION || stored != [assignment] {
            return Err(culprit_core::Error::invalid(culprit_core::t!("fleet.assignment_changed")));
        }
        Ok(Self { connection, _owner: owner })
    }
    fn load(&self, key: &str) -> Result<Checkpoint> {
        let json: Option<String> = self
            .connection
            .query_row("SELECT state_json FROM jobs WHERE id=?1", [key], |row| row.get(0))
            .optional()
            .map_err(|_| state_failed())?;
        match json {
            Some(json) if json.len() <= MAX_CHECKPOINT_BYTES => serde_json::from_str(&json).map_err(|_| state_failed()),
            Some(_) => Err(state_failed()),
            None => Ok(Checkpoint::default()),
        }
    }
    fn save(&self, key: &str, json: &str, gap: Option<FleetGap>) -> Result<()> {
        let transaction = self.connection.unchecked_transaction().map_err(|_| state_failed())?;
        transaction
            .execute(
                "INSERT INTO jobs VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json",
                params![key, json],
            )
            .map_err(|_| state_failed())?;
        if let Some(gap) = gap {
            let json = serde_json::to_string(&gap).map_err(|_| state_failed())?;
            transaction
                .execute(
                    "INSERT INTO gaps(id,event_json) VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",
                    params![gap.id, json],
                )
                .map_err(|_| state_failed())?;
        }
        transaction.commit().map_err(|_| state_failed())
    }
    fn gaps(&self, after: i64, limit: u32) -> Result<FleetGapPage> {
        let mut statement = self
            .connection
            .prepare("SELECT ordinal,event_json FROM gaps WHERE ordinal>?1 ORDER BY ordinal LIMIT ?2")
            .map_err(|_| state_failed())?;
        let rows = statement
            .query_map(params![after, limit + 1], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|_| state_failed())?;
        let mut page = FleetGapPage { gaps: Vec::new(), cursor: after.to_string(), more: false };
        let mut bytes = 0usize;
        for row in rows {
            let (ordinal, json) = row.map_err(|_| state_failed())?;
            if json.len() > MAX_CHECKPOINT_BYTES {
                return Err(state_failed());
            }
            if page.gaps.len() == limit as usize || bytes + json.len() > 4 * 1024 * 1024 {
                page.more = true;
                break;
            }
            bytes += json.len();
            page.gaps.push(serde_json::from_str(&json).map_err(|_| state_failed())?);
            page.cursor = ordinal.to_string();
        }
        Ok(page)
    }
}
