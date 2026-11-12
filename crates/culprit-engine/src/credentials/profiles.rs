//! Authentication composition: configuration holds references, and only the selected provider resolves credentials.
use culprit_auth::{BearerTokenProvider, CredentialResolver, DatabaseTokenProvider, PasswordCredential};
use culprit_auth_azure::{AzureCredential, IdentityConfig};
use culprit_core::{Error, ErrorCode, Result, t};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroizing;

const MAX_PROFILES: usize = 128;
const MAX_PROFILE_NAME_BYTES: usize = 128;
const MAX_USERNAME_BYTES: usize = 256;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CredentialProfile {
    Azure { identity: IdentityConfig },
    PasswordEnvironment { username: String, variable: String },
}
enum ConfiguredProfile {
    Azure(Arc<AzureCredential>),
    PasswordEnvironment { username: String, variable: String },
}
pub struct ConfiguredCredentials(BTreeMap<String, ConfiguredProfile>);
impl ConfiguredCredentials {
    pub fn new(profiles: &BTreeMap<String, CredentialProfile>) -> Result<Self> {
        if profiles.len() > MAX_PROFILES {
            return Err(invalid());
        }
        let profiles = profiles
            .iter()
            .map(|(name, profile)| {
                if name.is_empty()
                    || name.len() > MAX_PROFILE_NAME_BYTES
                    || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    return Err(invalid());
                }
                let profile = match profile {
                    CredentialProfile::Azure { identity } => {
                        ConfiguredProfile::Azure(Arc::new(AzureCredential::new(identity.clone())?))
                    }
                    CredentialProfile::PasswordEnvironment { username, variable } => {
                        if username.is_empty()
                            || username.len() > MAX_USERNAME_BYTES
                            || username.chars().any(char::is_control)
                            || !environment_name(variable)
                        {
                            return Err(invalid());
                        }
                        ConfiguredProfile::PasswordEnvironment {
                            username: username.clone(),
                            variable: variable.clone(),
                        }
                    }
                };
                Ok((name.clone(), profile))
            })
            .collect::<Result<_>>()?;
        Ok(Self(profiles))
    }
    fn profile(&self, name: &str) -> Result<&ConfiguredProfile> {
        self.0.get(name).ok_or_else(invalid)
    }
}
impl CredentialResolver for ConfiguredCredentials {
    fn bearer(&self, profile: &str) -> Result<Arc<dyn BearerTokenProvider>> {
        match self.profile(profile)? {
            ConfiguredProfile::Azure(credential) => Ok(credential.clone()),
            ConfiguredProfile::PasswordEnvironment { .. } => Err(invalid()),
        }
    }
    fn database(&self, profile: &str) -> Result<Arc<dyn DatabaseTokenProvider>> {
        match self.profile(profile)? {
            ConfiguredProfile::Azure(credential) => Ok(credential.clone()),
            ConfiguredProfile::PasswordEnvironment { .. } => Err(invalid()),
        }
    }
    fn validate_password(&self, profile: &str) -> Result<()> {
        match self.profile(profile)? {
            ConfiguredProfile::PasswordEnvironment { .. } => Ok(()),
            ConfiguredProfile::Azure(_) => Err(invalid()),
        }
    }
    fn password(&self, profile: &str) -> Result<PasswordCredential> {
        match self.profile(profile)? {
            ConfiguredProfile::PasswordEnvironment { username, variable } => {
                let password = std::env::var(variable).map_err(|_| {
                    Error::new(ErrorCode::AuthFailed, t!("engine.connection.password_env_unset", variable = variable))
                })?;
                Ok(PasswordCredential { username: username.clone(), password: Zeroizing::new(password) })
            }
            ConfiguredProfile::Azure(_) => Err(invalid()),
        }
    }
}
fn environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ENVIRONMENT_NAME_BYTES
        && name.bytes().enumerate().all(|(position, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (position > 0 && byte.is_ascii_digit())
        })
}
fn invalid() -> Error {
    Error::invalid(t!("engine.credentials.invalid_profile"))
}
