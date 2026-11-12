//! Bounded ClickHouse HTTP operations. Credentials are supplied by the engine and never serialized or diagnosed.
use crate::response::{Column, QueryTable};
use async_trait::async_trait;
use culprit_collector::{
    analytical::encoding::EncodedBatch,
    delivery::{contract::MAX_RANGE_BYTES, sink::Submission},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;
mod http;
#[cfg(test)]
mod tests;

const MAX_PARALLEL_REQUESTS: usize = 2;
const MAX_QUERY_ROWS: usize = 10_001;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_PARAMETERS: usize = 32;
const MAX_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickHouseConfig {
    pub endpoint: String,
    pub database: String,
    /// A replica quorum for deployments using ReplicatedMergeTree; omit for a single-node MergeTree deployment.
    #[serde(default)]
    pub insert_quorum: Option<u32>,
}
impl ClickHouseConfig {
    pub fn destination_id(&self) -> Result<String> {
        let origin = self.origin()?;
        let bytes = serde_json::to_vec(&(origin, &self.database, self.insert_quorum)).map_err(|_| invalid())?;
        Ok(format!("clickhouse:{}", culprit_core::telemetry::digest(&bytes)))
    }
    fn origin(&self) -> Result<String> {
        if self.database.is_empty()
            || self.database.len() > 256
            || self.database.chars().any(char::is_control)
            || self.insert_quorum.is_some_and(|n| n == 0 || n > 128)
            || self.endpoint.contains('#')
            || self.endpoint.chars().any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(invalid());
        }
        let uri: ureq::http::Uri = self.endpoint.parse().map_err(|_| invalid())?;
        let scheme = uri.scheme_str().ok_or_else(invalid)?;
        let authority = uri.authority().ok_or_else(invalid)?;
        if authority.as_str().contains('@') || uri.query().is_some() || (uri.path() != "/" && !uri.path().is_empty()) {
            return Err(invalid());
        }
        let host = authority.host().to_ascii_lowercase();
        let loopback = matches!(host.as_str(), "127.0.0.1" | "[::1]" | "localhost");
        if scheme != "https" && !(scheme == "http" && loopback) {
            return Err(invalid());
        }
        let default = if scheme == "https" { 443 } else { 80 };
        let suffix = authority.as_str().strip_prefix(authority.host()).ok_or_else(invalid)?;
        let port = if suffix.is_empty() {
            default
        } else {
            let port = suffix.strip_prefix(':').ok_or_else(invalid)?;
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            port.parse::<u16>().map_err(|_| invalid())?
        };
        if port == 0 || host.is_empty() || host.len() > 253 || host.contains(['\\', '%']) {
            return Err(invalid());
        }
        Ok(if port == default { format!("{scheme}://{host}") } else { format!("{scheme}://{host}:{port}") })
    }
}
pub struct Credentials {
    username: String,
    password: Zeroizing<String>,
}
impl Credentials {
    pub fn new(username: String, password: Zeroizing<String>) -> Result<Self> {
        if username.is_empty()
            || username.len() > 256
            || username.chars().any(char::is_control)
            || password.len() > 32 * 1024
            || password.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(Self { username, password })
    }
}
pub struct QueryRequest {
    pub text: String,
    pub parameters: BTreeMap<String, String>,
    pub columns: Vec<Column>,
    pub row_limit: usize,
}
impl QueryRequest {
    fn validate(&self) -> Result<()> {
        if self.text.is_empty()
            || self.text.len() > MAX_QUERY_BYTES
            || self.parameters.len() > MAX_PARAMETERS
            || self.columns.is_empty()
            || self.columns.len() > culprit_collector::analytical::schema::MAX_COLUMNS
            || self.row_limit == 0
            || self.row_limit > MAX_QUERY_ROWS
            || self.parameters.iter().any(|(name, value)| {
                !culprit_collector::analytical::schema::identifier(name) || value.len() > 64 * 1024
            })
            || self.text.len() + self.parameters.values().map(String::len).sum::<usize>() > MAX_REQUEST_BYTES
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub enum InsertData {
    Batch(Arc<EncodedBatch>),
    Control(Vec<u8>),
}
impl InsertData {
    fn bytes(&self) -> u64 {
        match self {
            Self::Batch(batch) => batch.bytes,
            Self::Control(bytes) => bytes.len() as u64,
        }
    }
}
#[async_trait]
pub trait QueryTransport: Send + Sync {
    fn destination(&self) -> &str;
    async fn query(&self, request: QueryRequest) -> Result<QueryTable>;
    async fn insert(&self, table: &str, data: InsertData) -> Result<Submission>;
}
pub struct ClickHouseClient {
    destination: String,
    origin: String,
    config: ClickHouseConfig,
    credentials: Arc<Credentials>,
    requests: Arc<Semaphore>,
}
impl ClickHouseClient {
    pub fn new(config: ClickHouseConfig, credentials: Credentials) -> Result<Self> {
        Ok(Self {
            destination: config.destination_id()?,
            origin: config.origin()?,
            config,
            credentials: Arc::new(credentials),
            requests: Arc::new(Semaphore::new(MAX_PARALLEL_REQUESTS)),
        })
    }
}
#[async_trait]
impl QueryTransport for ClickHouseClient {
    fn destination(&self) -> &str {
        &self.destination
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        request.validate()?;
        let permit = self.requests.clone().acquire_owned().await.map_err(|_| transport())?;
        let (origin, config, credentials) = (self.origin.clone(), self.config.clone(), self.credentials.clone());
        let id = request_id()?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            http::query(&origin, &config, &credentials, &id, request)
        })
        .await
        .map_err(|_| transport())?
    }
    async fn insert(&self, table: &str, data: InsertData) -> Result<Submission> {
        if !culprit_collector::analytical::schema::identifier(table) || data.bytes() > MAX_RANGE_BYTES {
            return Err(invalid());
        }
        if data.bytes() == 0 {
            return Ok(Submission::Accepted);
        }
        let permit = self.requests.clone().acquire_owned().await.map_err(|_| transport())?;
        let (origin, config, credentials, table) =
            (self.origin.clone(), self.config.clone(), self.credentials.clone(), table.to_owned());
        let id = request_id()?;
        // The worker retains the batch, its temporary file and its spool lease through native HTTP completion.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            http::insert(&origin, &config, &credentials, &id, &table, data)
        })
        .await
        .map_err(|_| transport())?
    }
}
fn request_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| transport())?;
    Ok(format!("culprit-{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()))
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}
fn transport() -> culprit_core::Error {
    DeliveryProblem::Transport.error()
}
