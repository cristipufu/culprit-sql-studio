use super::*;
use culprit_collector::delivery::object_store::transport::valid_object_key as object_key;
use serde::{Deserialize, Serialize};

/// One recovery namespace. Provision the private container separately with the deployment identity.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AzureBlobConfig {
    pub account: String,
    pub container: String,
    pub prefix: String,
}
impl AzureBlobConfig {
    pub fn access_scope(&self) -> Result<String> {
        self.endpoint()?;
        Ok(format!("azure-blob:{}/{}", self.account, self.container))
    }

    pub(super) fn endpoint(&self) -> Result<Endpoint> {
        if !(3..=24).contains(&self.account.len())
            || !self.account.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            || !(3..=63).contains(&self.container.len())
            || !self.container.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || self.container.starts_with('-')
            || self.container.ends_with('-')
            || self.container.contains("--")
            || !object_key(&self.prefix)
            || self.prefix.len() > 512
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(Endpoint {
            origin: format!("https://{}.blob.core.windows.net", self.account),
            path_prefix: format!("/{}/{}", self.container, self.prefix),
        })
    }
}
pub(super) struct Endpoint {
    pub origin: String,
    pub path_prefix: String,
}
impl Endpoint {
    pub fn path(&self, key: &str) -> Result<String> {
        if !object_key(key) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(format!("{}/{key}", self.path_prefix))
    }
}
