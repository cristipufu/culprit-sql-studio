//! Authenticated, read-only verification queries. Management and ingestion use separate contracts.
use super::response::{self, Column, QueryTable};
use crate::{
    authentication::{TokenProvider, kusto_origin},
    http,
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

#[cfg(test)]
mod tests;

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_PARAMETERS: usize = 32;
const MAX_QUERY_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

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
    database: String,
    service: http::Service,
}
impl AdxClient {
    /// Validates without network or token acquisition; destination outages cannot prevent source/recovery startup.
    pub fn new(config: AdxConfig, credential: Arc<dyn TokenProvider>) -> Result<Self> {
        Ok(Self {
            destination: config.destination_id()?,
            service: http::Service::new(&config.endpoint, credential)?,
            database: config.database,
        })
    }
}
#[async_trait]
impl QueryTransport for AdxClient {
    fn destination(&self) -> &str {
        &self.destination
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        let request_id = http::request_id()?;
        let body = request.encode(&self.database, &request_id)?;
        self.service
            .execute(move |origin, token| {
                http::query(origin, token, &request_id, body, &request.columns, request.row_limit)
            })
            .await
    }
}

#[cfg(test)]
pub(super) fn emulator_query(port: u16, request: QueryRequest) -> Result<QueryTable> {
    let id = http::request_id()?;
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
