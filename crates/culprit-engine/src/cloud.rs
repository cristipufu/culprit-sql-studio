//! Cloud adapter composition and private filesystem cache. Native discovery, metrics and assessment live outside the engine.
use crate::{ConnectionInfo, credentials, private_fs};
use culprit_cloud::{CloudConnection, CloudHttp, CloudServices, JsonReply, MetricsCache};
use culprit_core::{CloudSignals, Result};
use std::{path::Path, sync::Arc};

struct Http;
#[async_trait::async_trait]
impl CloudHttp for Http {
    async fn get_bearer(&self, url: &str, token: &str, max_bytes: u64) -> Result<JsonReply> {
        let reply = crate::http::get_bearer_limited(url.into(), token.into(), max_bytes).await?;
        Ok(JsonReply { status: reply.status, json: reply.json })
    }
}
pub(crate) fn services() -> CloudServices {
    let azure = Arc::new(culprit_cloud::AzureCloud::new(
        credentials::azure_commands(),
        Arc::new(credentials::azure_cli()),
        Arc::new(Http),
    ));
    let aws = Arc::new(culprit_cloud::AwsCloud::new(Arc::new(credentials::aws())));
    CloudServices::default()
        .with_discovery_provider(azure.clone())
        .with_metrics_provider(azure)
        .with_discovery_provider(aws.clone())
        .with_metrics_provider(aws)
}
pub(crate) fn metadata(info: &ConnectionInfo) -> CloudConnection {
    CloudConnection {
        name: info.name.clone(),
        engine: info.engine,
        host: info.host.clone(),
        database: info.database.clone(),
    }
}
struct Cache<'a>(&'a Path);
impl MetricsCache for Cache<'_> {
    fn read(&self) -> Option<Vec<u8>> {
        std::fs::read(self.0.join("cloud.json")).ok()
    }
    fn write(&self, bytes: &[u8]) {
        if private_fs::create_dir(self.0).is_ok() {
            let _ = private_fs::write_atomic(&self.0.join("cloud.json"), bytes);
        }
    }
}
pub(crate) async fn signals(
    services: &CloudServices,
    dir: &Path,
    info: &ConnectionInfo,
    start: f64,
    end: f64,
    refresh: bool,
) -> Result<CloudSignals> {
    services.signals(&Cache(dir), &metadata(info), start, end, refresh).await
}
