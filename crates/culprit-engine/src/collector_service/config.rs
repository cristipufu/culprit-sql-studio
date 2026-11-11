//! Static assignments and explicit source/recovery/target dependencies; configuration contains no credentials.
use crate::{adx::client::AdxConfig, azure_blob::AzureBlobConfig, azure_identity::IdentityConfig};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorConfig {
    pub pipeline: String,
    pub shard: String,
    pub workspace: PathBuf,
    pub state: PathBuf,
    pub inventory: FleetInventory,
    pub identity: IdentityConfig,
    pub recovery: RecoveryConfig,
    pub target: TargetConfig,
    pub limits: OutboxLimits,
    pub health_listen: std::net::SocketAddr,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RecoveryConfig {
    AzureBlob { storage: AzureBlobConfig },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TargetConfig {
    Adx { query: AdxConfig, ready: AzureBlobConfig },
}

impl CollectorConfig {
    pub fn from_file(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| invalid())?
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(invalid());
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        self.inventory.validate(&self.shard)?;
        for target in self.inventory.targets.iter().filter(|target| target.shard == self.shard) {
            for schedule in &target.schedules {
                let budget = &schedule.budget;
                // Refuse captures that cannot fit the archive contract before spending source work.
                if u64::from(budget.page_rows) * u64::from(budget.max_pages)
                    > culprit_collector::delivery::contract::MAX_RANGE_RECORDS as u64
                {
                    return Err(invalid());
                }
            }
        }
        if !self.workspace.is_absolute()
            || !self.state.is_absolute()
            || self.workspace.components().any(|part| matches!(part, std::path::Component::ParentDir))
            || self.state.components().any(|part| matches!(part, std::path::Component::ParentDir))
            || self.workspace.starts_with(&self.state)
            || self.state.starts_with(&self.workspace)
            || !self.limits.is_valid()
        {
            return Err(invalid());
        }
        let RecoveryConfig::AzureBlob { storage } = &self.recovery;
        let TargetConfig::Adx { ready, .. } = &self.target;
        // Container-scoped ingestion permissions must not grant access to the native, unredacted recovery data.
        if storage.account == ready.account && storage.container == ready.container {
            return Err(invalid());
        }
        Ok(())
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}
