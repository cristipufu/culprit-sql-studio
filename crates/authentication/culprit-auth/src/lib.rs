//! Credentials are resolved by the embedding runtime; adapters never discover credentials or log token contents.
use async_trait::async_trait;
use culprit_core::Result;
use zeroize::Zeroizing;

#[async_trait]
pub trait BearerTokenProvider: Send + Sync {
    async fn bearer_token(&self, resource: &str) -> Result<Zeroizing<String>>;
}

pub struct DatabaseTokenRequest<'a> {
    pub engine: culprit_core::Engine,
    pub host: &'a str,
    pub port: Option<u16>,
    pub username: Option<&'a str>,
    pub region: Option<&'a str>,
}
pub struct DatabaseCredential {
    pub token: Zeroizing<String>,
    pub username: Option<String>,
}
/// Native authentication owns its input requirements, principal metadata and host validation.
#[async_trait]
pub trait DatabaseTokenProvider: Send + Sync {
    fn auth_kind(&self) -> culprit_core::AuthKind;
    async fn database_token(&self, request: DatabaseTokenRequest<'_>) -> Result<DatabaseCredential>;
}

pub struct PasswordCredential {
    pub username: String,
    pub password: Zeroizing<String>,
}

/// Access-key credentials for request-signing protocols. Native adapters own the signing scheme and endpoint policy.
/// Neither credentials nor their providers are serializable or printable.
#[derive(Clone)]
pub struct AccessKeyCredential {
    pub key_id: Zeroizing<String>,
    pub secret_key: Zeroizing<String>,
    pub session_token: Option<Zeroizing<String>>,
    pub expires_at: Option<std::time::SystemTime>,
}
#[async_trait]
pub trait AccessKeyProvider: Send + Sync {
    async fn access_keys(&self) -> Result<AccessKeyCredential>;
}

/// Named profiles are selected by the host. Validation never acquires a token or reads a password.
pub trait CredentialResolver: Send + Sync {
    fn bearer(&self, profile: &str) -> Result<std::sync::Arc<dyn BearerTokenProvider>>;
    fn database(&self, profile: &str) -> Result<std::sync::Arc<dyn DatabaseTokenProvider>>;
    fn validate_password(&self, profile: &str) -> Result<()>;
    fn password(&self, profile: &str) -> Result<PasswordCredential>;
    fn access_keys(&self, _profile: &str) -> Result<std::sync::Arc<dyn AccessKeyProvider>> {
        Err(culprit_core::Error::invalid(culprit_core::t!("engine.credentials.invalid_profile")))
    }
}

/// A canonical DNS HTTPS origin. Adapter policy decides which hosts and resources it trusts.
pub fn https_origin(endpoint: &str) -> Option<String> {
    let host = endpoint.strip_prefix("https://")?;
    let host = host.strip_suffix('/').unwrap_or(host);
    if host.len() > 253
        || host.is_empty()
        || host.split('.').any(|part| {
            part.is_empty()
                || part.len() > 63
                || part.starts_with('-')
                || part.ends_with('-')
                || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    Some(format!("https://{}", host.to_ascii_lowercase()))
}

use culprit_core::Error;
use std::{collections::HashMap, sync::Mutex};
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    /// Deleting something that isn't there is not an error.
    fn delete(&self, account: &str) -> Result<()>;
    /// Drop any copy kept in this process (the server rejected it), so the next `get` reads the backing store again.
    fn forget(&self, _account: &str) {}
}

/// Secrets that live as long as the process. Nothing touches the OS keychain.
#[derive(Default)]
pub struct Memory(Mutex<HashMap<String, String>>);

impl SecretStore for Memory {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().map_err(|_| Error::internal("secret store poisoned"))?.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.0.lock().map_err(|_| Error::internal("secret store poisoned"))?.insert(account.into(), secret.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().map_err(|_| Error::internal("secret store poisoned"))?.remove(account);
        Ok(())
    }
}

/// The host owns executable discovery, bounded output and cancellation of the complete process tree.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn output(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Result<std::process::Output>;
}

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ts_rs::TS;
/// What the person needs to finish a device-code sign-in.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    /// The identity provider's instructions, e.g. "To sign in, use a web browser to open the page … and enter the code …".
    pub message: String,
    #[ts(type = "number")]
    pub expires_in_secs: u64,
}

/// Device-code sign-in progress, for whoever shows it (the CLI prints, the app opens a dialog).
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
#[ts(export)]
pub enum SignInEvent {
    Code { prompt: DeviceCodePrompt },
    Done,
    Failed { message: String },
}

pub type SignInHandler = Arc<dyn Fn(SignInEvent) + Send + Sync>;

/// A native authentication adapter, including any interactive lifecycle it owns. Headless credential providers
/// remain separate: resolving an unattended profile must never start an interactive sign-in.
#[async_trait]
pub trait DatabaseAuthentication: Send + Sync {
    fn auth_kind(&self) -> culprit_core::AuthKind;
    async fn credential(
        &self,
        connection: &culprit_core::ConnectionSpec,
        secrets: &dyn SecretStore,
    ) -> Result<DatabaseCredential>;
    fn set_sign_in_handler(&self, _handler: Option<SignInHandler>) {}
    fn cancel_sign_in(&self) {}
    fn sign_out(&self, _connections: &[culprit_core::ConnectionSpec], _secrets: &dyn SecretStore) -> Result<usize> {
        Ok(0)
    }
}

#[async_trait]
pub trait AuthenticationService: Send + Sync {
    async fn credential(
        &self,
        connection: &culprit_core::ConnectionSpec,
        secrets: &dyn SecretStore,
    ) -> Result<DatabaseCredential>;
    fn set_sign_in_handler(&self, handler: Option<SignInHandler>);
    fn cancel_sign_in(&self);
    fn sign_out(&self, connections: &[culprit_core::ConnectionSpec], secrets: &dyn SecretStore) -> Result<usize>;
}

/// An authenticated native command client. Only the selected adapter interprets native CLI failures.
#[async_trait]
pub trait CommandClient: Send + Sync {
    async fn run(&self, arguments: &[&str]) -> Result<Vec<u8>>;
}
