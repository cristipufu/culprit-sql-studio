//! Only these fixed identity endpoints receive assertions. Response/error bodies never become diagnostics.
use super::*;
use std::{io::Read, sync::OnceLock};
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY_BYTES: u64 = 64 * 1024;
const MAX_ASSERTION_BYTES: u64 = 32 * 1024;
static IDENTITY_REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
pub(super) struct IdentityRequest {
    pub config: IdentityConfig,
    pub audience: Audience,
}
pub(super) struct IdentityReply {
    pub status: u16,
    pub json: serde_json::Value,
}
#[async_trait::async_trait]
pub(super) trait IdentityEndpoint: Send + Sync {
    async fn request(&self, request: IdentityRequest) -> Result<IdentityReply>;
}
pub(super) struct HttpEndpoint;
#[async_trait::async_trait]
impl IdentityEndpoint for HttpEndpoint {
    async fn request(&self, request: IdentityRequest) -> Result<IdentityReply> {
        let permit = IDENTITY_REQUESTS.acquire().await.map_err(|_| transport_error())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let response = match request.config {
                IdentityConfig::WorkloadIdentity { tenant_id, client_id, token_file } => {
                    let assertion = read_assertion(&token_file)?;
                    let scope = format!("{}/.default", request.audience.resource().trim_end_matches('/'));
                    agent(false)
                        .post(format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token"))
                        .send_form([
                            ("client_id", client_id.as_str()),
                            ("scope", scope.as_str()),
                            ("grant_type", "client_credentials"),
                            ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
                            ("client_assertion", assertion.as_str()),
                        ])
                }
                IdentityConfig::ManagedIdentity { client_id } => {
                    let mut call = agent(true)
                        .get("http://169.254.169.254/metadata/identity/oauth2/token")
                        .header("Metadata", "true")
                        .query("api-version", "2018-02-01")
                        .query("resource", request.audience.resource());
                    if let Some(client_id) = client_id {
                        call = call.query("client_id", client_id);
                    }
                    call.call()
                }
            };
            let mut response = response.map_err(|_| transport_error())?;
            let status = response.status().as_u16();
            let bytes = response
                .body_mut()
                .with_config()
                .limit(MAX_REPLY_BYTES)
                .read_to_vec()
                .map_err(|_| transport_error())?;
            let json = serde_json::from_slice(&bytes).map_err(|_| authentication_error())?;
            Ok(IdentityReply { status, json })
        })
        .await
        .map_err(|_| transport_error())?
    }
}
fn agent(metadata: bool) -> &'static ureq::Agent {
    static LOGIN: OnceLock<ureq::Agent> = OnceLock::new();
    static METADATA: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = if metadata { &METADATA } else { &LOGIN };
    agent.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(!metadata)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(IDENTITY_TIMEOUT))
            .build()
            .into()
    })
}
// Projected Kubernetes tokens rotate by replacing a symlink. Reopen on every refresh, with a complete bounded read.
pub(super) fn read_assertion(path: &std::path::Path) -> Result<Zeroizing<String>> {
    let file = std::fs::File::open(path).map_err(|_| assertion_error())?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(assertion_error());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_ASSERTION_BYTES + 1).read_to_end(&mut bytes).map_err(|_| assertion_error())?;
    if bytes.len() as u64 > MAX_ASSERTION_BYTES {
        return Err(assertion_error());
    }
    let token = std::str::from_utf8(&bytes).map_err(|_| assertion_error())?.trim();
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace()) {
        return Err(assertion_error());
    }
    Ok(Zeroizing::new(token.to_string()))
}
fn assertion_error() -> Error {
    Error::new(ErrorCode::AuthFailed, t!("engine.identity.assertion_unavailable"))
}
fn transport_error() -> Error {
    Error::new(ErrorCode::ConnectionFailed, t!("engine.identity.transport_failed"))
}
