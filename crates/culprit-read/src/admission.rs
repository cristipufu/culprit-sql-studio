//! Durable workspace admission. Authentication may be cancelled; native work stays blocked until explicitly settled.
mod gate;
use crate::durable_fs;
use culprit_core::{
    Error, ErrorCode, Result,
    telemetry::{CollectionAdmissionStatus, CollectionStop},
};
pub use gate::{SourceGate, SourcePermit};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

/// A new observation is distinct from retries of retained evidence.
pub fn observation_identity() -> Result<culprit_core::telemetry::CaptureIdentity> {
    Ok(culprit_core::telemetry::CaptureIdentity { observation_id: durable_fs::random_id()?, revision: None })
}

const FORMAT: u32 = 1;
const MAX_STATE_BYTES: usize = 16 * 1024;
const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = "owner.lock";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum Work {
    #[default]
    Quiet,
    Started {
        observation_id: String,
        connection: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    format: u32,
    work: Work,
}
struct Journal {
    root: PathBuf,
    work: Work,
    _owner: durable_fs::ExclusiveFile,
}
#[derive(Clone)]
pub struct SourceAdmission {
    journal: Arc<Mutex<Journal>>,
}

impl SourceAdmission {
    pub async fn acquire(root: PathBuf) -> Result<Self> {
        let guard = Self::open(root).await?;
        if guard.status().await.source_quiescence_required {
            return Err(unconfirmed());
        }
        Ok(guard)
    }
    async fn open(root: PathBuf) -> Result<Self> {
        let journal = tokio::task::spawn_blocking(move || Journal::open(root)).await.map_err(|_| storage())??;
        Ok(Self { journal: Arc::new(Mutex::new(journal)) })
    }
    pub async fn started(&self, observation_id: String, connection: String) -> Result<()> {
        if !valid_observation(&observation_id) || connection.is_empty() || connection.len() > 512 {
            return Err(storage());
        }
        let mut journal = self.journal.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if matches!(journal.work, Work::Started { .. }) {
                return Err(unconfirmed());
            }
            journal.write(Work::Started { observation_id, connection })
        })
        .await
        .map_err(|_| storage())?
    }
    /// Only the native owner's confirmed closure permits this transition; dropping a guard never clears debt.
    pub async fn confirmed_quiet(&self) -> Result<()> {
        self.write(Work::Quiet).await
    }
    async fn write(&self, work: Work) -> Result<()> {
        // Hold the mutation lock before dispatch, so a cancelled write cannot race a later quiet acknowledgement.
        let mut journal = self.journal.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || journal.write(work)).await.map_err(|_| storage())?
    }
    pub async fn status(&self) -> CollectionAdmissionStatus {
        status(&self.journal.lock().await.work)
    }
    pub async fn inspect(root: PathBuf) -> Result<CollectionAdmissionStatus> {
        tokio::task::spawn_blocking(move || read(&root).map(|work| status(&work))).await.map_err(|_| storage())?
    }
    /// An operator must verify the named observation's native work has stopped. This performs no database queries.
    pub async fn acknowledge(root: PathBuf, observation_id: &str) -> Result<CollectionAdmissionStatus> {
        let guard = Self::open(root).await?;
        let current = guard.status().await;
        if current.observation_id.as_deref() != Some(observation_id) {
            return Err(Error::invalid(culprit_core::t!("collection.admission.wrong_observation")));
        }
        guard.confirmed_quiet().await?;
        Ok(guard.status().await)
    }
}
impl Journal {
    fn open(root: PathBuf) -> Result<Self> {
        create_directory(&root)?;
        match durable_fs::create_file(&root.join(LOCK_FILE)) {
            Ok(file) => file.sync_all().map_err(|_| storage())?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return Err(storage()),
        }
        durable_fs::sync_directory(&root).map_err(|_| storage())?;
        let owner = durable_fs::own_file(&root.join(LOCK_FILE)).map_err(|error| match error {
            durable_fs::OwnershipError::Contended => {
                Error::new(ErrorCode::QueryFailed, culprit_core::t!("collection.admission.busy"))
            }
            _ => storage(),
        })?;
        let work = read(&root)?;
        Ok(Self { root, work, _owner: owner })
    }
    fn write(&mut self, work: Work) -> Result<()> {
        let path = self.root.join(STATE_FILE);
        let temporary = path.with_file_name(format!(".{STATE_FILE}.new"));
        // Exclusive ownership makes an abandoned publication temporary safe to remove. Never recover its contents.
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(&temporary).map_err(|_| storage())?,
            Ok(_) => return Err(storage()),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(storage()),
        }
        let bytes = durable_fs::json_bytes(&Stored { format: FORMAT, work: work.clone() }, MAX_STATE_BYTES)
            .map_err(|_| storage())?;
        durable_fs::publish(&path, &bytes).map_err(|_| storage())?;
        self.work = work;
        Ok(())
    }
}
fn read(root: &Path) -> Result<Work> {
    let path = root.join(STATE_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Work::Quiet),
        Ok(metadata) if metadata.file_type().is_file() => {}
        _ => return Err(storage()),
    }
    let mut bytes = Vec::new();
    durable_fs::regular_file(&path)
        .map_err(|_| storage())?
        .take((MAX_STATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| storage())?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(storage());
    }
    let stored: Stored = serde_json::from_slice(&bytes).map_err(|_| storage())?;
    if stored.format != FORMAT {
        return Err(storage());
    }
    if let Work::Started { observation_id, connection } = &stored.work
        && (!valid_observation(observation_id) || connection.is_empty() || connection.len() > 512)
    {
        return Err(storage());
    }
    Ok(stored.work)
}
fn create_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        _ => return Err(storage()),
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    create_directory(parent)?;
    match durable_fs::create_directory(path) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == ErrorKind::AlreadyExists
                && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir()) =>
        {
            Ok(())
        }
        Err(_) => Err(storage()),
    }
}
fn status(work: &Work) -> CollectionAdmissionStatus {
    match work {
        Work::Quiet => {
            CollectionAdmissionStatus { observation_id: None, connection: None, source_quiescence_required: false }
        }
        Work::Started { observation_id, connection } => CollectionAdmissionStatus {
            observation_id: Some(observation_id.clone()),
            connection: Some(connection.clone()),
            source_quiescence_required: true,
        },
    }
}
fn valid_observation(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn storage() -> Error {
    Error::new(ErrorCode::QueryFailed, culprit_core::t!("collection.admission.storage"))
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::StorageFailed))
}
fn unconfirmed() -> Error {
    CollectionStop::CancellationUnconfirmed.error().with_hint(culprit_core::t!("collection.admission.recover_hint"))
}

#[cfg(test)]
mod tests;
