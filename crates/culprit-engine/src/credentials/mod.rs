//! Credential composition; native acquisition belongs to the authentication provider crates.
#[cfg(feature = "collector")]
mod profiles;
use crate::{
    private_fs,
    program::{self, Cli},
};
use culprit_auth::CommandRunner;
use culprit_core::Result;
#[cfg(feature = "collector")]
pub use profiles::{ConfiguredCredentials, CredentialProfile};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct CloudCommand(Cli);
#[async_trait::async_trait]
impl CommandRunner for CloudCommand {
    async fn output(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Result<std::process::Output> {
        let mut command = program::command(self.0)?;
        command.args(arguments).envs(environment.iter().copied());
        crate::cloud_process::output(command).await
    }
}
pub(crate) fn entra() -> culprit_auth_azure::entra::EntraAuth {
    culprit_auth_azure::entra::EntraAuth::new(Arc::new(CloudCommand(Cli::Azure)))
}
pub(crate) fn aws() -> culprit_auth_aws::AwsCli {
    culprit_auth_aws::AwsCli::new(Arc::new(CloudCommand(Cli::Aws)))
}
/// Write the bundled RDS certificate authorities next to Culprit's data, once, and return the file.
pub(crate) fn rds_ca_file(data_dir: &Path) -> Result<PathBuf> {
    let dir = data_dir.join("certs");
    let path = dir.join("rds-global-bundle.pem");
    let current = std::fs::read(&path).ok();
    if current.as_deref() != Some(culprit_auth_aws::rds_ca_bundle()) {
        private_fs::create_dir(&dir).map_err(|e| culprit_core::Error::internal(format!("{}: {e}", dir.display())))?;
        private_fs::write_atomic(&path, culprit_auth_aws::rds_ca_bundle())
            .map_err(|e| culprit_core::Error::internal(format!("{}: {e}", path.display())))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
