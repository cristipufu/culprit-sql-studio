//! Authenticated, read-only verification queries. Management and ingestion use separate contracts.
use super::response::{self, Column, QueryTable};
use crate::authentication::{Audience, TokenProvider, kusto_origin};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{OnceCell, Semaphore};
use zeroize::Zeroizing;

mod http;
#[cfg(test)]
mod tests;

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_PARAMETERS: usize = 32;
const MAX_PARALLEL_QUERIES: usize = 2;
const MAX_QUERY_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
static METADATA_REQUESTS: Semaphore = Semaphore::const_new(2);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdxConfig {
    pub endpoint: String,
    pub database: String,
}
impl AdxConfig {
    pub fn destination_id(&self) -> Result<String> {
        let origin = self.validated()?;
        let bytes =
            serde_json::to_vec(&(origin, &self.database)).map_err(|_| DeliveryProblem::InvalidContract.error())?;
        Ok(format!("adx:{}", culprit_core::telemetry::digest(&bytes)))
    }
    fn validated(&self) -> Result<String> {
        if self.database.is_empty() || self.database.len() > 256 || self.database.chars().any(char::is_control) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        kusto_origin(&self.endpoint)
    }
}

pub struct QueryRequest {
    pub text: String,
    /// REST accepts strings and signed integer values directly. Other declared parameter types use KQL literals.
    pub parameters: BTreeMap<String, Value>,
    pub columns: Vec<Column>,
    pub row_limit: usize,
}
impl QueryRequest {
    fn encode(&self, database: &str, request_id: &str) -> Result<Vec<u8>> {
        if self.text.is_empty()
            || self.text.len() > MAX_QUERY_BYTES
            || self.parameters.len() > MAX_PARAMETERS
            || self.columns.is_empty()
            || self.columns.len() > 128
            || self.row_limit == 0
            || self.row_limit > 10_001
            || self.parameters.iter().any(|(key, value)| !parameter_name(key) || !(value.is_string() || value.is_i64()))
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let properties = json!({ "Options": {
            "queryconsistency": "strongconsistency", "query_results_cache_max_age": "00:00:00",
            "results_progressive_enabled": false, "results_v2_fragment_primary_tables": false,
            "results_error_reporting_placement": "end_of_dataset", "servertimeout": "00:00:30",
            "truncationmaxrecords": 10_001, "truncationmaxsize": response::MAX_RESPONSE_BYTES,
            "maxmemoryconsumptionperiterator": MAX_QUERY_MEMORY_BYTES,
            "max_memory_consumption_per_query_per_node": MAX_QUERY_MEMORY_BYTES,
            "request_readonly": true, "request_readonly_hardline": true, "request_callout_disabled": true
        }, "Parameters": self.parameters, "ClientRequestId": request_id });
        let body = json!({ "db": database, "csl": self.text, "properties": properties.to_string() });
        let bytes = serde_json::to_vec(&body).map_err(|_| DeliveryProblem::Integrity.error())?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(bytes)
    }
}
fn parameter_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[async_trait]
pub trait QueryTransport: Send + Sync {
    fn destination(&self) -> &str;
    async fn query(&self, request: QueryRequest) -> Result<QueryTable>;
}

pub struct AdxClient {
    destination: String,
    origin: String,
    database: String,
    audience: OnceCell<Audience>,
    credential: Arc<dyn TokenProvider>,
    queries: Arc<Semaphore>,
}
impl AdxClient {
    /// Validate without network or token acquisition. A destination outage cannot prevent source/recovery startup.
    pub fn new(config: AdxConfig, credential: Arc<dyn TokenProvider>) -> Result<Self> {
        let origin = config.validated()?;
        let destination = config.destination_id()?;
        Ok(Self {
            destination,
            origin,
            database: config.database,
            audience: OnceCell::new(),
            credential,
            queries: Arc::new(Semaphore::new(MAX_PARALLEL_QUERIES)),
        })
    }
    /// Reads only same-origin public authentication metadata. No credentials accompany discovery.
    async fn audience(&self) -> Result<&Audience> {
        self.audience
            .get_or_try_init(|| async {
                let origin = self.origin.clone();
                let permit = METADATA_REQUESTS.acquire().await.map_err(|_| DeliveryProblem::Transport.error())?;
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    http::discover(&origin)
                })
                .await
                .map_err(|_| DeliveryProblem::Transport.error())?
            })
            .await
    }
}
#[async_trait]
impl QueryTransport for AdxClient {
    fn destination(&self) -> &str {
        &self.destination
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        let request_id = request_id()?;
        let body = request.encode(&self.database, &request_id)?;
        let permit = self.queries.clone().acquire_owned().await.map_err(|_| DeliveryProblem::Transport.error())?;
        let token = self.credential.bearer_token(self.audience().await?.resource()).await?;
        let origin = self.origin.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            http::query(&origin, &token, &request_id, body, &request.columns, request.row_limit)
        })
        .await
        .map_err(|_| DeliveryProblem::Transport.error())?
    }
}
fn request_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| DeliveryProblem::Transport.error())?;
    Ok(format!("Culprit.Delivery;{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()))
}

#[cfg(test)]
pub(super) fn emulator_query(port: u16, request: QueryRequest) -> Result<QueryTable> {
    let id = request_id()?;
    let body = request.encode("CulpritFixture", &id)?;
    http::query(
        &format!("http://127.0.0.1:{port}"),
        "owned-emulator-only",
        &id,
        body,
        &request.columns,
        request.row_limit,
    )
}
