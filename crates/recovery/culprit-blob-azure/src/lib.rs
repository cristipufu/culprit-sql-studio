//! Azure Blob recovery transport. Tokens never leave this crate or appear in errors.
//! REST contracts: https://learn.microsoft.com/rest/api/storageservices/put-blob
//! and https://learn.microsoft.com/rest/api/storageservices/get-blob.
mod config;
mod http;
use culprit_collector::delivery::object_store::bridge as reader;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
pub use config::AzureBlobConfig;
use config::Endpoint;
use culprit_auth::BearerTokenProvider;
use culprit_collector::delivery::{
    contract::{MAX_MANIFEST_BYTES, MAX_RANGE_BYTES},
    object_store::transport::*,
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
#[cfg(test)]
use tokio::sync::mpsc;
use zeroize::Zeroizing;

const STORAGE_VERSION: &str = "2023-11-03";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(test)]
use reader::DOWNLOAD_CHUNK_BYTES;
const MAX_PARALLEL_REQUESTS: usize = 4;

enum Authorization {
    Azure(Arc<dyn BearerTokenProvider>),
    #[cfg(test)]
    Emulator {
        account: String,
        key: Zeroizing<Vec<u8>>,
    },
}
pub struct AzureBlobTransport {
    endpoint: Arc<Endpoint>,
    authorization: Authorization,
    requests: Arc<Semaphore>,
}
impl AzureBlobTransport {
    pub fn new(config: AzureBlobConfig, credential: Arc<dyn BearerTokenProvider>) -> Result<Self> {
        Ok(Self {
            endpoint: Arc::new(config.endpoint()?),
            authorization: Authorization::Azure(credential),
            requests: Arc::new(Semaphore::new(MAX_PARALLEL_REQUESTS)),
        })
    }
    async fn authorization(&self) -> Result<RequestAuthorization> {
        match &self.authorization {
            Authorization::Azure(credential) => {
                Ok(RequestAuthorization::Bearer(credential.bearer_token("https://storage.azure.com/").await?))
            }
            #[cfg(test)]
            Authorization::Emulator { account, key } => {
                Ok(RequestAuthorization::SharedKey { account: account.clone(), key: key.clone() })
            }
        }
    }
    async fn upload(&self, key: &str, body: Upload, condition: Condition) -> Result<ObjectWrite> {
        let permit = self.requests.clone().acquire_owned().await.map_err(|_| transport_error())?;
        let path = self.endpoint.path(key)?;
        let request = http::BlobRequest::new(self.endpoint.clone(), path, self.authorization().await?);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            request.upload(body, condition)
        })
        .await
        .map_err(|_| transport_error())?
    }
}
#[async_trait]
impl ObjectTransport for AzureBlobTransport {
    async fn get(&self, key: &str) -> Result<Option<ObjectRead>> {
        let path = self.endpoint.path(key)?;
        let request = http::BlobRequest::new(self.endpoint.clone(), path, self.authorization().await?);
        reader::download(self.requests.clone(), move || {
            request.download().map(|download| {
                download.map(|(response, etag, bytes)| reader::Download {
                    etag,
                    bytes,
                    body: Box::new(response.into_body().into_reader()),
                })
            })
        })
        .await
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<ObjectWrite> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.upload(key, Upload::Bytes(bytes), Condition::from(condition)).await
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<ObjectWrite> {
        if bytes > MAX_RANGE_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        let file = reader::open_upload(path, bytes)?;
        self.upload(key, Upload::File { file, bytes }, Condition::Absent).await
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
}
enum Condition {
    Absent,
    Matches(String),
}
impl From<WriteCondition<'_>> for Condition {
    fn from(value: WriteCondition<'_>) -> Self {
        match value {
            WriteCondition::Absent => Self::Absent,
            WriteCondition::Matches(etag) => Self::Matches(etag.into()),
        }
    }
}
enum RequestAuthorization {
    Bearer(Zeroizing<String>),
    #[cfg(test)]
    SharedKey {
        account: String,
        key: Zeroizing<Vec<u8>>,
    },
}
impl RequestAuthorization {
    fn header(
        &self,
        method: &str,
        path: &str,
        bytes: u64,
        condition: Option<&Condition>,
        date: &str,
    ) -> Zeroizing<String> {
        match self {
            Self::Bearer(token) => {
                let _ = (method, path, bytes, condition, date);
                Zeroizing::new(format!("Bearer {}", token.as_str()))
            }
            #[cfg(test)]
            Self::SharedKey { account, key } => tests::shared_key(account, key, method, path, bytes, condition, date),
        }
    }
}
fn response_metadata(response: &ureq::http::Response<ureq::Body>) -> Result<(String, u64)> {
    let headers = response.headers();
    if headers.contains_key("Content-Range") || headers.get("Content-Encoding").is_some_and(|value| value != "identity")
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    let etag = headers
        .get("ETag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(transport_error)?
        .to_owned();
    let bytes = headers
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(transport_error)?;
    if bytes > MAX_RANGE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok((etag, bytes))
}
fn storage_date() -> String {
    chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}
fn transport_error() -> culprit_core::Error {
    DeliveryProblem::Transport.error()
}
fn agent(secure: bool) -> &'static ureq::Agent {
    static AZURE: OnceLock<ureq::Agent> = OnceLock::new();
    static EMULATOR: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = if secure { &AZURE } else { &EMULATOR };
    agent.get_or_init(|| {
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

#[cfg(test)]
#[path = "../../../../tests/support/http.rs"]
mod http_fixture;

/// Native recovery registration; archive and warehouse policy remain in the shared collector.
pub struct AzureRecoveryProvider;
impl culprit_collector::delivery::object_store::provider::RecoveryProvider for AzureRecoveryProvider {
    fn name(&self) -> &str {
        "azure-blob"
    }
    fn configure(
        &self,
        options: &serde_json::Value,
        profile: &str,
        credentials: &dyn culprit_auth::CredentialResolver,
    ) -> Result<culprit_collector::delivery::object_store::provider::RecoveryTarget> {
        let config: AzureBlobConfig =
            serde_json::from_value(options.clone()).map_err(|_| DeliveryProblem::InvalidContract.error())?;
        let access_scope = config.access_scope()?;
        let credential = credentials.bearer(profile)?;
        Ok(culprit_collector::delivery::object_store::provider::RecoveryTarget {
            transport: Arc::new(AzureBlobTransport::new(config, credential)?),
            access_scope,
        })
    }
}
