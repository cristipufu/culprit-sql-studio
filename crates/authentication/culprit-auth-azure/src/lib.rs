//! Explicit unattended Azure credentials. No CLI, device-code, refresh-token or keychain fallback.
use culprit_core::{Engine, Error, ErrorCode, Result, t};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use zeroize::Zeroizing;

mod endpoint;
#[cfg(test)]
mod tests;
use endpoint::{IdentityEndpoint, IdentityRequest};

const MAX_AUDIENCES: usize = 16;
const MAX_CONFIGURATION_BYTES: u64 = 16 * 1024;
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);
const MAX_TOKEN_BYTES: usize = 32 * 1024;
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum IdentityConfig {
    WorkloadIdentity { tenant_id: String, client_id: String, token_file: PathBuf },
    ManagedIdentity { client_id: Option<String> },
}
impl IdentityConfig {
    fn validate(&self) -> Result<()> {
        match self {
            Self::WorkloadIdentity { tenant_id, client_id, token_file }
                if is_guid(tenant_id) && is_guid(client_id) && token_file.is_absolute() =>
            {
                Ok(())
            }
            Self::ManagedIdentity { client_id } if client_id.as_deref().is_none_or(is_guid) => Ok(()),
            _ => Err(configuration_error()),
        }
    }
}
/// An Entra resource, never a caller-supplied token URL. Kusto resources come from validated service metadata.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Audience(String);
impl Audience {
    pub fn database(engine: Engine) -> Result<Self> {
        database_resource(engine).map(|value| Self(value.into())).ok_or_else(configuration_error)
    }
    pub fn storage() -> Self {
        Self("https://storage.azure.com/".into())
    }
    pub fn monitor() -> Self {
        Self("https://monitor.azure.com/".into())
    }
    pub fn arm() -> Self {
        Self("https://management.azure.com/".into())
    }
    pub fn resource(&self) -> &str {
        &self.0
    }
}
pub fn database_resource(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Mssql => Some("https://database.windows.net/"),
        Engine::Postgres | Engine::Mysql => Some("https://ossrdbms-aad.database.windows.net"),
        Engine::Yugabyte | Engine::Sqlite => None,
    }
}

struct AccessToken {
    value: Zeroizing<String>,
    refresh_at: Instant,
}
#[derive(Default)]
struct CachedIdentity {
    token: Option<AccessToken>,
    failure: Option<(Instant, Error)>,
}
type AudienceCache = Arc<tokio::sync::Mutex<CachedIdentity>>;

/// One configured identity owns its audience caches. Changing identity requires a new credential owner.
pub struct AzureCredential {
    config: IdentityConfig,
    endpoint: Arc<dyn IdentityEndpoint>,
    audiences: Mutex<BTreeMap<Audience, AudienceCache>>,
}
impl AzureCredential {
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|_| configuration_error())?;
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err(configuration_error());
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIGURATION_BYTES + 1).read_to_end(&mut bytes).map_err(|_| configuration_error())?;
        if bytes.len() as u64 > MAX_CONFIGURATION_BYTES {
            return Err(configuration_error());
        }
        let config = serde_json::from_slice(&bytes).map_err(|_| configuration_error())?;
        Self::new(config)
    }
    pub fn new(config: IdentityConfig) -> Result<Self> {
        Self::with_endpoint(config, Arc::new(endpoint::HttpEndpoint))
    }
    fn with_endpoint(config: IdentityConfig, endpoint: Arc<dyn IdentityEndpoint>) -> Result<Self> {
        config.validate()?;
        Ok(Self { config, endpoint, audiences: Mutex::new(BTreeMap::new()) })
    }
    pub async fn token(&self, audience: &Audience) -> Result<String> {
        let cache = self.audience(audience)?;
        let mut cache = cache.lock().await;
        if let Some(token) = &cache.token
            && Instant::now() < token.refresh_at
        {
            return Ok(token.value.to_string());
        }
        if let Some((at, failure)) = &cache.failure
            && at.elapsed() < FAILURE_BACKOFF
        {
            return Err(failure.clone());
        }
        let started = Instant::now();
        let request = IdentityRequest { config: self.config.clone(), audience: audience.clone() };
        let result = self.endpoint.request(request).await.and_then(|reply| parse_token(reply, audience, started));
        match result {
            Ok(token) => {
                let value = token.value.to_string();
                cache.token = Some(token);
                cache.failure = None;
                Ok(value)
            }
            Err(error) => {
                cache.failure = Some((Instant::now(), error.clone()));
                Err(error)
            }
        }
    }
    fn audience(&self, audience: &Audience) -> Result<AudienceCache> {
        let mut caches = self.audiences.lock().map_err(|_| configuration_error())?;
        if !caches.contains_key(audience) && caches.len() >= MAX_AUDIENCES {
            return Err(configuration_error());
        }
        Ok(caches.entry(audience.clone()).or_default().clone())
    }
    /// Host validation applies before acquisition, including when a token already exists in memory.
    pub async fn database_token(&self, engine: Engine, host: &str) -> Result<String> {
        if !culprit_core::entra_host_allowed(engine, host) {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                t!("engine.entra.host_refused", host = host, engine = engine.label()),
            ));
        }
        self.token(&Audience::database(engine)?).await
    }
}

fn parse_token(reply: endpoint::IdentityReply, audience: &Audience, started: Instant) -> Result<AccessToken> {
    let json = reply.json;
    if reply.status != 200
        || !json
            .get("token_type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("Bearer"))
    {
        return Err(authentication_error());
    }
    if json.get("resource").and_then(serde_json::Value::as_str).is_some_and(|resource| resource != audience.resource())
    {
        return Err(authentication_error());
    }
    let value = json
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_TOKEN_BYTES
                && !value.bytes().any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        })
        .ok_or_else(authentication_error)?;
    let seconds =
        |value: &serde_json::Value| value.as_u64().or_else(|| value.as_str().and_then(|value| value.parse().ok()));
    let mut lifetime =
        json.get("expires_in").and_then(seconds).map(Duration::from_secs).ok_or_else(authentication_error)?;
    if let Some(expires_on) = json.get("expires_on") {
        let expires_on = expires_on
            .as_u64()
            .or_else(|| expires_on.as_str().and_then(|value| value.parse().ok()))
            .ok_or_else(authentication_error)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| authentication_error())?
            .as_secs();
        lifetime = lifetime.min(Duration::from_secs(expires_on.saturating_sub(now)));
    }
    if lifetime.is_zero() || lifetime > MAX_TOKEN_LIFETIME {
        return Err(authentication_error());
    }
    let refresh_at = started + lifetime - REFRESH_MARGIN.min(lifetime / 5);
    if Instant::now() >= refresh_at {
        return Err(authentication_error());
    }
    Ok(AccessToken { value: Zeroizing::new(value.to_owned()), refresh_at })
}
fn configuration_error() -> Error {
    Error::invalid(t!("engine.identity.invalid_configuration"))
}
fn authentication_error() -> Error {
    Error::new(ErrorCode::AuthFailed, t!("engine.identity.authentication_failed"))
}

#[async_trait::async_trait]
impl culprit_auth::BearerTokenProvider for AzureCredential {
    async fn bearer_token(&self, resource: &str) -> Result<Zeroizing<String>> {
        let fixed = [Audience::storage(), Audience::arm(), Audience::monitor()];
        let audience = if let Some(audience) = fixed.into_iter().find(|a| a.resource() == resource) {
            audience
        } else {
            let origin = culprit_auth::https_origin(resource).ok_or_else(configuration_error)?;
            if origin != resource || !origin.ends_with(".kusto.windows.net") {
                return Err(configuration_error());
            }
            Audience(origin)
        };
        self.token(&audience).await.map(Zeroizing::new)
    }
}
#[async_trait::async_trait]
impl culprit_auth::DatabaseTokenProvider for AzureCredential {
    fn auth_kind(&self) -> culprit_core::AuthKind {
        culprit_core::AuthKind::Entra
    }
    async fn database_token(
        &self,
        request: culprit_auth::DatabaseTokenRequest<'_>,
    ) -> Result<culprit_auth::DatabaseCredential> {
        let token = AzureCredential::database_token(self, request.engine, request.host).await?;
        Ok(database_credential(request.engine, token))
    }
}

pub fn is_guid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

pub mod entra;
pub mod interactive_http;

fn database_credential(engine: Engine, token: String) -> culprit_auth::DatabaseCredential {
    let username = if engine == Engine::Mssql { None } else { entra::token_user(&token) };
    culprit_auth::DatabaseCredential { token: Zeroizing::new(token), username }
}
