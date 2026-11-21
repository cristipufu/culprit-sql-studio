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

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_collector::analytical::test_support::{
        QualificationFixture, QualificationFixtureCase, baseline_profile_expectation,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[tokio::test]
    async fn retirement_observed_by_the_outer_gate_cannot_be_healed_before_the_native_guard_or_close() {
        let fixture = QualificationFixture::baseline_activity(QualificationFixtureCase::Unconfirmed);
        let request = fixture.request();
        let context = NativeReadContext::from_activity(&fixture.source).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let flag = active.clone();
        let admission = baseline_profile_expectation(&request, &context, move || flag.load(Ordering::Acquire));
        let cloned = admission.clone();
        let mut source = ProfileSource::new(fixture.telemetry_source(), admission);
        active.store(false, Ordering::Release);
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
        active.store(true, Ordering::Release);
        assert!(!cloned.approval_is_active());
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
        assert_eq!(source.query_count(), 0);
        assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
        assert!(fixture.closed());
    }
}
