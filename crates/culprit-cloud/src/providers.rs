//! Native Azure and AWS cloud adapters implement the same independent discovery and metrics contracts.
use crate::*;
use culprit_auth::{BearerTokenProvider, CommandClient, CommandRunner};

pub struct AzureCloud {
    pub(crate) commands: Arc<dyn CommandRunner>,
    pub(crate) tokens: Arc<dyn BearerTokenProvider>,
    pub(crate) http: Arc<dyn CloudHttp>,
}
impl AzureCloud {
    pub fn new(
        commands: Arc<dyn CommandRunner>,
        tokens: Arc<dyn BearerTokenProvider>,
        http: Arc<dyn CloudHttp>,
    ) -> Self {
        Self { commands, tokens, http }
    }
}
pub struct AwsCloud {
    pub(crate) commands: Arc<dyn CommandClient>,
}
impl AwsCloud {
    pub fn new(commands: Arc<dyn CommandClient>) -> Self {
        Self { commands }
    }
}

#[async_trait]
impl DiscoveryProvider for AzureCloud {
    fn cloud(&self) -> Cloud {
        Cloud::Azure
    }
    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryResult> {
        discovery::azure(self, &request.terms).await
    }
}
#[async_trait]
impl DiscoveryProvider for AwsCloud {
    fn cloud(&self) -> Cloud {
        Cloud::Aws
    }
    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryResult> {
        discovery::aws_rds(self, &request.regions, &request.terms).await
    }
}
#[async_trait]
impl MetricsProvider for AzureCloud {
    fn name(&self) -> &str {
        "azure-monitor-v1"
    }
    fn recognizes(&self, connection: &CloudConnection) -> bool {
        matches!(
            metrics::target(connection),
            Some(metrics::Target::AzureSql { .. } | metrics::Target::AzureFlexible { .. })
        )
    }
    async fn resolve(&self, connection: &CloudConnection) -> Result<Value> {
        let target = metrics::target(connection).ok_or_else(invalid)?;
        let resource = match &target {
            metrics::Target::AzureSql { server, database } => {
                metrics::azure::resolve_azure_sql(self, server, database).await?
            }
            metrics::Target::AzureFlexible { engine, server } => {
                metrics::azure::resolve_azure_flexible(self, *engine, server).await?
            }
            _ => return Err(invalid()),
        };
        serde_json::to_value((target, resource)).map_err(|_| invalid())
    }
    async fn read(&self, metadata: &Value, start: f64, end: f64) -> Result<culprit_core::CloudSignals> {
        let (target, resource): (metrics::Target, metrics::Resource) =
            serde_json::from_value(metadata.clone()).map_err(|_| invalid())?;
        if !matches!(target, metrics::Target::AzureSql { .. } | metrics::Target::AzureFlexible { .. }) {
            return Err(invalid());
        }
        metrics::azure::azure_signals(self, &target, &resource, start, end).await
    }
}
#[async_trait]
impl MetricsProvider for AwsCloud {
    fn name(&self) -> &str {
        "cloudwatch-rds-v1"
    }
    fn recognizes(&self, connection: &CloudConnection) -> bool {
        matches!(metrics::target(connection), Some(metrics::Target::Rds { .. }))
    }
    async fn resolve(&self, connection: &CloudConnection) -> Result<Value> {
        let Some(metrics::Target::Rds { region, id, cluster, reader }) = metrics::target(connection) else {
            return Err(invalid());
        };
        let resource = metrics::aws::resolve_rds(self, &region, &id, cluster, reader).await?;
        serde_json::to_value(resource).map_err(|_| invalid())
    }
    async fn read(&self, metadata: &Value, start: f64, end: f64) -> Result<culprit_core::CloudSignals> {
        let resource: metrics::Resource = serde_json::from_value(metadata.clone()).map_err(|_| invalid())?;
        metrics::aws::rds_signals(self, &resource, start, end).await
    }
}
fn invalid() -> Error {
    Error::invalid(t!("engine.credentials.invalid_profile"))
}
