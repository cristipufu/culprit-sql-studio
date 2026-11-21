use super::*;
use culprit_cloud::{Cloud, DiscoveryProvider, DiscoveryRequest, DiscoveryResult};
use culprit_core::Engine;
use std::sync::{Arc, Mutex};

fn connection(host: &str) -> CloudConnection {
    CloudConnection {
        name: "fixture".into(),
        engine: Engine::Postgres,
        host: host.into(),
        database: Some("fixture".into()),
    }
}
#[test]
fn builtin_metric_registration_follows_compiled_capabilities_without_credentials_or_network() {
    let services = services();
    assert_eq!(services.recognizes(&connection("fixture.postgres.database.azure.com")), cfg!(feature = "auth-azure"));
    assert_eq!(
        services.recognizes(&connection("fixture.abc.eu-central-1.rds.amazonaws.com")),
        cfg!(feature = "auth-aws")
    );
    assert!(!services.recognizes(&connection("ordinary-database.invalid")));
}
#[cfg(not(feature = "auth-azure"))]
#[tokio::test]
async fn disabled_azure_discovery_is_explicitly_unavailable_without_builtin_authentication() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = crate::Workspace::open_in(directory.path());
    let error = workspace.discover_azure(&[]).await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
}
#[cfg(not(feature = "auth-aws"))]
#[tokio::test]
async fn disabled_aws_discovery_is_explicitly_unavailable_without_builtin_authentication() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = crate::Workspace::open_in(directory.path());
    let error = workspace.discover_aws(&[], &[]).await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
}
struct Discovery {
    cloud: Cloud,
    requests: Arc<Mutex<Vec<Cloud>>>,
}
#[async_trait::async_trait]
impl DiscoveryProvider for Discovery {
    fn cloud(&self) -> Cloud {
        self.cloud
    }
    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryResult> {
        assert!(request.terms.is_empty());
        self.requests.lock().unwrap().push(self.cloud);
        Ok(DiscoveryResult { warnings: vec!["owned discovery response".into()], ..Default::default() })
    }
}
#[tokio::test]
async fn injected_discovery_uses_the_existing_workspace_api_with_or_without_builtins() {
    let directory = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let workspace = crate::Workspace::open_in(directory.path())
        .with_discovery_provider(Arc::new(Discovery { cloud: Cloud::Azure, requests: requests.clone() }))
        .with_discovery_provider(Arc::new(Discovery { cloud: Cloud::Aws, requests: requests.clone() }));
    assert_eq!(workspace.discover_azure(&[]).await.unwrap().warnings, ["owned discovery response"]);
    assert_eq!(workspace.discover_aws(&[], &[]).await.unwrap().warnings, ["owned discovery response"]);
    assert_eq!(*requests.lock().unwrap(), [Cloud::Azure, Cloud::Aws]);
}
