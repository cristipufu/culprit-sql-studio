use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn native_provider_selection_uses_metadata_without_authentication_or_network_work() {
    let services = CloudServices::default()
        .with_metrics_provider(Arc::new(test_support::azure()))
        .with_metrics_provider(Arc::new(test_support::aws()));
    for (host, expected) in [
        ("fixture.database.windows.net", true),
        ("fixture.abc.us-east-1.rds.amazonaws.com", true),
        ("fixture.invalid", false),
    ] {
        assert_eq!(
            services.recognizes(&CloudConnection {
                name: "fixture".into(),
                engine: Engine::Mssql,
                host: host.into(),
                database: Some("shop".into())
            }),
            expected
        );
    }
}
struct Discovery(Arc<AtomicUsize>);
#[async_trait]
impl DiscoveryProvider for Discovery {
    fn cloud(&self) -> Cloud {
        Cloud::Azure
    }
    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryResult> {
        assert_eq!(request.terms, ["fixture"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(DiscoveryResult::default())
    }
}
#[derive(Default)]
struct Cache(Mutex<Option<Vec<u8>>>);
impl MetricsCache for Cache {
    fn read(&self) -> Option<Vec<u8>> {
        self.0.lock().unwrap().clone()
    }
    fn write(&self, bytes: &[u8]) {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
    }
}
struct Metrics {
    name: &'static str,
    resolves: AtomicUsize,
    reads: AtomicUsize,
}
#[async_trait]
impl MetricsProvider for Metrics {
    fn name(&self) -> &str {
        self.name
    }
    fn recognizes(&self, _: &CloudConnection) -> bool {
        true
    }
    async fn resolve(&self, _: &CloudConnection) -> Result<Value> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({"opaque":"native-metadata"}))
    }
    async fn read(&self, metadata: &Value, start: f64, end: f64) -> Result<culprit_core::CloudSignals> {
        assert_eq!(metadata["opaque"], "native-metadata");
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(culprit_core::CloudSignals {
            provider: culprit_core::cloud::CloudProvider::Azure,
            service: "fixture".into(),
            resource: "fixture".into(),
            tier: None,
            start: metrics::iso(start),
            end: metrics::iso(end),
            period_seconds: 60,
            series: vec![],
            episodes: vec![],
            summary: String::new(),
            notes: vec![],
            fetched_at: metrics::iso(end),
        })
    }
}
#[tokio::test]
async fn injected_roles_are_independent_and_cached_metadata_is_owned_by_the_metrics_provider() {
    let discoveries = Arc::new(AtomicUsize::new(0));
    let first = Arc::new(Metrics { name: "first", resolves: AtomicUsize::new(0), reads: AtomicUsize::new(0) });
    let services = CloudServices::default()
        .with_discovery_provider(Arc::new(Discovery(discoveries.clone())))
        .with_metrics_provider(first.clone());
    let cache = Cache::default();
    let connection = CloudConnection {
        name: "fixture".into(),
        engine: Engine::Sqlite,
        host: "external-provider".into(),
        database: None,
    };
    let end = chrono::Utc::now().timestamp() as f64;
    let start = end - 3600.0;
    services.signals(&cache, &connection, start, end, false).await.unwrap();
    services.signals(&cache, &connection, start, end, false).await.unwrap();
    assert_eq!(first.resolves.load(Ordering::SeqCst), 1);
    assert_eq!(first.reads.load(Ordering::SeqCst), 1);
    assert_eq!(discoveries.load(Ordering::SeqCst), 0);
    services
        .discover(Cloud::Azure, &DiscoveryRequest { terms: vec!["fixture".into()], regions: vec![] })
        .await
        .unwrap();
    assert_eq!(discoveries.load(Ordering::SeqCst), 1);
    let second = Arc::new(Metrics { name: "second", resolves: AtomicUsize::new(0), reads: AtomicUsize::new(0) });
    CloudServices::default()
        .with_metrics_provider(second.clone())
        .signals(&cache, &connection, start, end, false)
        .await
        .unwrap();
    assert_eq!(second.resolves.load(Ordering::SeqCst), 1);
    assert_eq!(second.reads.load(Ordering::SeqCst), 1);
}
