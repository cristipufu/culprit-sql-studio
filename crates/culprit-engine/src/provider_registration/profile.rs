//! Revocation stops new source calls; the original source still owns in-flight work and every close outcome.
use culprit_core::{Result, telemetry::*};

pub(super) struct ProfileSource {
    source: Box<dyn TelemetrySource>,
    admission: SourceProfileAdmission,
}
impl ProfileSource {
    pub fn new(source: Box<dyn TelemetrySource>, admission: SourceProfileAdmission) -> Self {
        Self { source, admission }
    }
    fn check_approval(&self) -> Result<()> {
        if self.admission.approval_is_active() { Ok(()) } else { Err(CollectionStop::SourceChanged.error()) }
    }
}
#[async_trait::async_trait]
impl TelemetrySource for ProfileSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.check_approval()?;
        self.source.describe().await
    }
    fn query_count(&self) -> u32 {
        self.source.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.source.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.check_approval()?;
        self.source.read_page(stream, cursor).await
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.check_approval()?;
        self.source.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        self.source.close().await
    }
}
