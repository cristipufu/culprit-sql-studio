//! Azure Blob recovery transport. Tokens never leave this crate or appear in errors.
//! REST contracts: https://learn.microsoft.com/rest/api/storageservices/put-blob
//! and https://learn.microsoft.com/rest/api/storageservices/get-blob.
mod config;
mod http;
mod reader;
#[cfg(test)]
mod tests;

use crate::azure_identity::{Audience, AzureCredential};
use async_trait::async_trait;
pub use config::AzureBlobConfig;
use config::Endpoint;
use culprit_collector::delivery::{
    blob_store::transport::*,
    contract::{MAX_MANIFEST_BYTES, MAX_RANGE_BYTES},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc, oneshot};
use zeroize::Zeroizing;

const STORAGE_VERSION: &str = "2023-11-03";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
const MAX_PARALLEL_REQUESTS: usize = 4;

enum Authorization {
    Azure(Arc<AzureCredential>),
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
    pub fn new(config: AzureBlobConfig, credential: Arc<AzureCredential>) -> Result<Self> {
        Ok(Self {
            endpoint: Arc::new(config.endpoint()?),
            authorization: Authorization::Azure(credential),
            requests: Arc::new(Semaphore::new(MAX_PARALLEL_REQUESTS)),
        })
    }
    async fn authorization(&self) -> Result<RequestAuthorization> {
        match &self.authorization {
            Authorization::Azure(credential) => {
                Ok(RequestAuthorization::Bearer(Zeroizing::new(credential.token(&Audience::storage()).await?)))
            }
            #[cfg(test)]
            Authorization::Emulator { account, key } => {
                Ok(RequestAuthorization::SharedKey { account: account.clone(), key: key.clone() })
            }
        }
    }
    async fn upload(&self, key: &str, body: Upload, condition: Condition) -> Result<BlobWrite> {
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
impl BlobTransport for AzureBlobTransport {
    async fn get(&self, key: &str) -> Result<Option<BlobRead>> {
        let permit = self.requests.clone().acquire_owned().await.map_err(|_| transport_error())?;
        let path = self.endpoint.path(key)?;
        let request = http::BlobRequest::new(self.endpoint.clone(), path, self.authorization().await?);
        let (metadata, reply) = oneshot::channel();
        let (chunks, receiver) = mpsc::channel(2);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match request.download() {
                Ok(Some((response, etag, bytes))) => {
                    if metadata.send(Ok(Some((etag, bytes)))).is_ok() {
                        reader::stream(response.into_body().into_reader(), bytes, chunks);
                    }
                }
                Ok(None) => {
                    let _ = metadata.send(Ok(None));
                }
                Err(error) => {
                    let _ = metadata.send(Err(error));
                }
            }
        });
        let Some((etag, bytes)) = reply.await.map_err(|_| transport_error())?? else {
            return Ok(None);
        };
        Ok(Some(BlobRead { etag, bytes, body: Box::pin(reader::DownloadReader::new(receiver)) }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.upload(key, Upload::Bytes(bytes), Condition::from(condition)).await
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<BlobWrite> {
        if bytes > MAX_RANGE_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.upload(key, Upload::File { path: path.into(), bytes }, Condition::Absent).await
    }
}
enum Upload {
    Bytes(Vec<u8>),
    File { path: PathBuf, bytes: u64 },
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
