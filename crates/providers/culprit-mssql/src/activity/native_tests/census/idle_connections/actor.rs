//! One fixture-only SqlClient process; the provider keeps its existing Tiberius reader.
use super::*;
use std::{io::Read, path::PathBuf, process::Stdio};
use tokio::process::{Child, Command};

const READY_BOUND: Duration = Duration::from_secs(10);
const ACTOR_BOUND: Duration = Duration::from_secs(50);
const MAX_ACTOR_METADATA_BYTES: usize = 4096;

pub(super) struct Actor {
    child: Child,
    directory: tempfile::TempDir,
    deadline: Instant,
    ready_metadata: Option<serde_json::Value>,
    completed_metadata: Option<serde_json::Value>,
}
pub(super) struct Identity {
    pub mars: NativeActivityIdentity,
    pub foreign: NativeActivityIdentity,
}
impl Actor {
    pub fn start(fixture: &OwnedSourceFixture) -> Result<Self> {
        let dll = PathBuf::from(std::env::var_os("CULPRIT_CENSUS_MARS_ACTOR_DLL").ok_or_else(invalid_data)?);
        if !dll.is_absolute() || !dll.is_file() {
            return Err(invalid_data());
        }
        let directory = tempfile::tempdir().map_err(|_| invalid_data())?;
        let child = Command::new("dotnet")
            .arg(dll)
            .env("CULPRIT_CENSUS_MARS_READY", directory.path().join("ready.json"))
            .env("CULPRIT_CENSUS_MARS_COMPLETED", directory.path().join("completed.json"))
            .env("CULPRIT_CENSUS_MARS_IDLE", directory.path().join("idle.json"))
            .env("CULPRIT_CENSUS_MARS_RELEASE", directory.path().join("release"))
            .env("CULPRIT_MSSQL_SOURCE_TEST_PORT", fixture.port.to_string())
            .env("CULPRIT_SMOKE_DATABASE", &fixture.database)
            .env("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD", fixture.password())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| invalid_data())?;
        Ok(Self {
            child,
            directory,
            deadline: Instant::now() + ACTOR_BOUND,
            ready_metadata: None,
            completed_metadata: None,
        })
    }
    pub async fn ready(&mut self, cancellation: &CollectionCancellation) -> Result<Identity> {
        let until = Instant::now() + READY_BOUND;
        loop {
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            if self.directory.path().join("ready.json").exists() {
                let value = self.metadata("ready.json")?;
                self.ready_metadata = Some(value.clone());
                if value["state"] != "ready" || value["actor_queries"] != 5 {
                    return Err(invalid_data());
                }
                return Ok(Identity { mars: identity(&value["mars"])?, foreign: identity(&value["foreign"])? });
            }
            if self.child.try_wait().map_err(|_| invalid_data())?.is_some() || Instant::now() >= until {
                return Err(CollectionStop::TimedOut.error());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub async fn idle(&mut self, cancellation: &CollectionCancellation) -> Result<()> {
        loop {
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            if self.directory.path().join("idle.json").exists() {
                let value = self.metadata("idle.json")?;
                return if value["state"] == "idle" && value["actor_queries"] == 5 {
                    Ok(())
                } else {
                    Err(invalid_data())
                };
            }
            if self.child.try_wait().map_err(|_| invalid_data())?.is_some() || Instant::now() >= self.deadline {
                return Err(CollectionStop::TimedOut.error());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub async fn finish(&mut self) -> Result<serde_json::Value> {
        std::fs::write(self.directory.path().join("release"), b"release").map_err(|_| invalid_data())?;
        let status = tokio::time::timeout_at(self.deadline, self.child.wait())
            .await
            .map_err(|_| CollectionStop::TimedOut.error())?
            .map_err(|_| invalid_data())?;
        let receipt = self.metadata("completed.json")?;
        self.completed_metadata = Some(receipt.clone());
        if !status.success()
            || receipt["completed"] != true
            || receipt["closed"] != true
            || receipt["actor_queries"] != 5
        {
            return Err(invalid_data());
        }
        Ok(receipt)
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"ready":self.ready_metadata,"completed":self.completed_metadata})
    }
    fn metadata(&self, name: &str) -> Result<serde_json::Value> {
        let file = std::fs::File::open(self.directory.path().join(name)).map_err(|_| invalid_data())?;
        let mut bytes = Vec::new();
        file.take(MAX_ACTOR_METADATA_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| invalid_data())?;
        if bytes.len() > MAX_ACTOR_METADATA_BYTES {
            return Err(invalid_data());
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid_data())
    }
}
fn identity(value: &serde_json::Value) -> Result<NativeActivityIdentity> {
    let id = value["session"].as_str().ok_or_else(invalid_data)?;
    if !id.parse::<u16>().is_ok_and(|id| id > 0 && id <= i16::MAX as u16) {
        return Err(invalid_data());
    }
    Ok(NativeActivityIdentity {
        id: id.into(),
        generation: crate::sql_time::native_time(value["generation"].as_str().ok_or_else(invalid_data)?)?,
    })
}
