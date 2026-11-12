//! Cloud discovery and metrics are independent provider roles. Native adapters receive bounded host transports;
//! they never discover executables, identities or secret stores.
pub mod discovery;
pub mod metrics;
pub mod providers;
use async_trait::async_trait;
use culprit_core::{Engine, Error, ErrorCode, Result, t};
pub use discovery::{Cloud, DiscoveredDatabase, DiscoveryResult};
pub use providers::{AwsCloud, AzureCloud};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CloudConnection {
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub database: Option<String>,
}
pub struct JsonReply {
    pub status: u16,
    pub json: Value,
}
#[async_trait]
pub trait CloudHttp: Send + Sync {
    async fn get_bearer(&self, url: &str, token: &str, max_bytes: u64) -> Result<JsonReply>;
}
#[derive(Default)]
pub struct DiscoveryRequest {
    pub terms: Vec<String>,
    pub regions: Vec<String>,
}
#[async_trait]
pub trait DiscoveryProvider: Send + Sync {
    fn cloud(&self) -> Cloud;
    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryResult>;
}
/// Resolved metadata is private to its provider. Consumers store and return it without interpreting native fields.
#[async_trait]
pub trait MetricsProvider: Send + Sync {
    fn name(&self) -> &str;
    fn recognizes(&self, connection: &CloudConnection) -> bool;
    async fn resolve(&self, connection: &CloudConnection) -> Result<Value>;
    async fn read(&self, metadata: &Value, start: f64, end: f64) -> Result<culprit_core::CloudSignals>;
}
pub trait MetricsCache: Send + Sync {
    fn read(&self) -> Option<Vec<u8>>;
    fn write(&self, bytes: &[u8]);
}
#[derive(Default)]
pub struct CloudServices {
    discovery: Vec<Arc<dyn DiscoveryProvider>>,
    metrics: Vec<Arc<dyn MetricsProvider>>,
}
impl CloudServices {
    pub fn with_discovery_provider(mut self, provider: Arc<dyn DiscoveryProvider>) -> Self {
        self.discovery.retain(|current| current.cloud() != provider.cloud());
        self.discovery.push(provider);
        self
    }
    pub fn with_metrics_provider(mut self, provider: Arc<dyn MetricsProvider>) -> Self {
        self.metrics.retain(|current| current.name() != provider.name());
        self.metrics.push(provider);
        self
    }
    pub async fn discover(&self, cloud: Cloud, request: &DiscoveryRequest) -> Result<DiscoveryResult> {
        self.discovery
            .iter()
            .find(|provider| provider.cloud() == cloud)
            .ok_or_else(|| Error::new(ErrorCode::Unsupported, t!("engine.discover.unavailable")))?
            .discover(request)
            .await
    }
    pub fn recognizes(&self, connection: &CloudConnection) -> bool {
        self.metrics.iter().any(|provider| provider.recognizes(connection))
    }
    pub async fn signals(
        &self,
        cache: &dyn MetricsCache,
        connection: &CloudConnection,
        start: f64,
        end: f64,
        refresh: bool,
    ) -> Result<culprit_core::CloudSignals> {
        let provider = self.metrics.iter().find(|provider| provider.recognizes(connection)).ok_or_else(|| {
            Error::new(ErrorCode::Unsupported, t!("engine.cloud.not_cloud", name = &connection.name))
                .with_hint(t!("engine.cloud.not_cloud_hint"))
        })?;
        metrics::signals(provider.as_ref(), cache, connection, start, end, refresh).await
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
