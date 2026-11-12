//! Conditional S3 object transport. Credentials are injected; the adapter never discovers identities.
mod config;
mod signing;
#[cfg(test)]
mod tests;
use async_trait::async_trait;
use config::Endpoint;
pub use config::S3Config;
use culprit_auth::{AccessKeyCredential, AccessKeyProvider, CredentialResolver};
use culprit_collector::delivery::{
    contract::{MAX_MANIFEST_BYTES, MAX_RANGE_BYTES},
    object_store::{
        bridge,
        provider::{RecoveryProvider, RecoveryTarget},
        transport::*,
    },
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{
    io::{Read, Seek},
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PARALLEL_REQUESTS: usize = 4;
pub struct S3Transport {
    endpoint: Arc<Endpoint>,
    credentials: Arc<dyn AccessKeyProvider>,
    requests: Arc<Semaphore>,
}
impl S3Transport {
    pub fn new(config: S3Config, credentials: Arc<dyn AccessKeyProvider>) -> Result<Self> {
        Ok(Self {
            endpoint: Arc::new(config.endpoint()?),
            credentials,
            requests: Arc::new(Semaphore::new(MAX_PARALLEL_REQUESTS)),
        })
    }
    async fn upload(&self, key: &str, body: Upload, condition: OwnedCondition) -> Result<ObjectWrite> {
        let path = self.endpoint.path(key)?;
        let credentials = self.credentials.access_keys().await?;
        let endpoint = self.endpoint.clone();
        let permit = self.requests.clone().acquire_owned().await.map_err(|_| transport())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            write(&endpoint, &path, body, condition, credentials)
        })
        .await
        .map_err(|_| transport())?
    }
}
#[async_trait]
impl ObjectTransport for S3Transport {
    async fn get(&self, key: &str) -> Result<Option<ObjectRead>> {
        let path = self.endpoint.path(key)?;
        let credentials = self.credentials.access_keys().await?;
        let endpoint = self.endpoint.clone();
        bridge::download(self.requests.clone(), move || {
            let headers =
                signing::sign("GET", &path, &endpoint, &credentials, &signing::sha256(b""), None, chrono::Utc::now())?;
            let mut request = agent(endpoint.origin.starts_with("https://"))
                .get(format!("{}{path}", endpoint.origin))
                .header("accept-encoding", "identity");
            for (key, value) in &headers.headers {
                request = request.header(key, value.as_str());
            }
            let response = request.call().map_err(|_| transport())?;
            match response.status().as_u16() {
                404 => Ok(None),
                200 => {
                    let (etag, bytes) = metadata(&response)?;
                    Ok(Some(bridge::Download { etag, bytes, body: Box::new(response.into_body().into_reader()) }))
                }
                _ => Err(transport()),
            }
        })
        .await
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<ObjectWrite> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.upload(key, Upload::Bytes(bytes), condition.into()).await
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<ObjectWrite> {
        if bytes > MAX_RANGE_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        // Open before yielding: a cancelled caller may retire its temporary pathname while the native worker owns it.
        let file = bridge::open_upload(path, bytes)?;
        self.upload(key, Upload::File { file, bytes }, OwnedCondition::Absent).await
    }
}
enum OwnedCondition {
    Absent,
    Matches(String),
}
impl From<WriteCondition<'_>> for OwnedCondition {
    fn from(value: WriteCondition<'_>) -> Self {
        match value {
            WriteCondition::Absent => Self::Absent,
            WriteCondition::Matches(etag) => Self::Matches(etag.into()),
        }
    }
}
enum Upload {
    Bytes(Vec<u8>),
    File { file: std::fs::File, bytes: u64 },
}
impl Upload {
    fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::File { bytes, .. } => *bytes,
        }
    }
    fn sha256(&mut self) -> Result<String> {
        match self {
            Self::Bytes(bytes) => Ok(signing::sha256(bytes)),
            Self::File { file, bytes } => {
                use sha2::{Digest, Sha256};
                let mut hash = Sha256::new();
                let mut read = 0u64;
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let count = file.read(&mut buffer).map_err(|_| transport())?;
                    if count == 0 {
                        break;
                    }
                    read += count as u64;
                    if read > *bytes {
                        return Err(integrity());
                    }
                    hash.update(&buffer[..count]);
                }
                if read != *bytes {
                    return Err(integrity());
                }
                file.rewind().map_err(|_| transport())?;
                Ok(signing::hex(&hash.finalize()))
            }
        }
    }
}
fn write(
    endpoint: &Endpoint,
    path: &str,
    mut body: Upload,
    condition: OwnedCondition,
    credentials: AccessKeyCredential,
) -> Result<ObjectWrite> {
    let precondition = match &condition {
        OwnedCondition::Absent => ("if-none-match", "*"),
        OwnedCondition::Matches(etag)
            if !etag.is_empty() && etag.len() <= 256 && !etag.bytes().any(|b| b.is_ascii_control()) =>
        {
            ("if-match", etag.as_str())
        }
        OwnedCondition::Matches(_) => return Err(integrity()),
    };
    let headers =
        signing::sign("PUT", path, endpoint, &credentials, &body.sha256()?, Some(precondition), chrono::Utc::now())?;
    let mut request = agent(endpoint.origin.starts_with("https://"))
        .put(format!("{}{path}", endpoint.origin))
        .header("content-length", body.len().to_string())
        .header("content-type", "application/octet-stream");
    for (key, value) in &headers.headers {
        request = request.header(key, value.as_str());
    }
    let response = match body {
        Upload::Bytes(bytes) => request.send(bytes),
        Upload::File { file, bytes } => request.send(ureq::SendBody::from_owned_reader(file.take(bytes))),
    }
    .map_err(|_| transport())?;
    match response.status().as_u16() {
        200 => Ok(ObjectWrite::Written),
        412 => Ok(ObjectWrite::ConditionNotMet),
        // A racing delete/conditional write has no commit proof. The shared protocol reconciles before another attempt.
        409 => Err(DeliveryProblem::Conflict.error()),
        _ => Err(transport()),
    }
}
fn metadata(response: &ureq::http::Response<ureq::Body>) -> Result<(String, u64)> {
    let headers = response.headers();
    if headers.contains_key("content-range") || headers.get("content-encoding").is_some_and(|v| v != "identity") {
        return Err(integrity());
    }
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(integrity)?
        .to_string();
    let bytes = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(integrity)?;
    if bytes > MAX_RANGE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok((etag, bytes))
}
fn agent(secure: bool) -> &'static ureq::Agent {
    static SECURE: OnceLock<ureq::Agent> = OnceLock::new();
    static LOOPBACK: OnceLock<ureq::Agent> = OnceLock::new();
    (if secure { &SECURE } else { &LOOPBACK }).get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(secure)
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into()
    })
}
pub struct S3RecoveryProvider;
impl RecoveryProvider for S3RecoveryProvider {
    fn name(&self) -> &str {
        "s3"
    }
    fn configure(
        &self,
        options: &serde_json::Value,
        profile: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<RecoveryTarget> {
        let config: S3Config =
            serde_json::from_value(options.clone()).map_err(|_| DeliveryProblem::InvalidContract.error())?;
        let access_scope = config.access_scope()?;
        Ok(RecoveryTarget {
            transport: Arc::new(S3Transport::new(config, credentials.access_keys(profile)?)?),
            access_scope,
        })
    }
}
fn transport() -> culprit_core::Error {
    DeliveryProblem::Transport.error()
}
fn integrity() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
#[path = "../../../../tests/support/http.rs"]
mod http_fixture;
