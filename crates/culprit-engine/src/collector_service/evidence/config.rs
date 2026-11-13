//! Query-only archive configuration has no source inventory, workspace, ingestion or staging dependencies.
use super::*;
use crate::credentials::{ConfiguredCredentials, CredentialProfile};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceConfig {
    pub pipeline: String,
    pub shard: String,
    pub credentials: BTreeMap<String, CredentialProfile>,
    pub recovery: super::super::config::RecoveryConfig,
    pub warehouse: EvidenceStoreConfig,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceStoreConfig {
    pub provider: String,
    pub options: serde_json::Value,
    pub credential: String,
}

impl EvidenceConfig {
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
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }

    pub(super) fn credentials(&self) -> Result<ConfiguredCredentials> {
        let mut selected = BTreeMap::new();
        for name in [&self.recovery.credential, &self.warehouse.credential] {
            let profile = self.credentials.get(name).ok_or_else(invalid)?;
            selected.insert(name.clone(), profile.clone());
        }
        ConfiguredCredentials::new(&selected)
    }
}
