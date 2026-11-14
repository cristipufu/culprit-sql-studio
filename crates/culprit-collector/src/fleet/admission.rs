//! A durable reservation grants a bounded start window; waiting on disk cannot authorize a late native start.
use super::{FleetBackend, scheduler::ADMISSION_START_WINDOW};
use crate::{capture::CollectionCancellation, outbox::Outbox};
use culprit_core::{Result, telemetry::*};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;

pub(super) struct AdmittedBackend<'a> {
    backend: &'a dyn FleetBackend,
    start_before: Instant,
    dispatched: AtomicBool,
}
impl<'a> AdmittedBackend<'a> {
    pub fn new(backend: &'a dyn FleetBackend, admitted_at: Instant) -> Self {
        Self { backend, start_before: admitted_at + ADMISSION_START_WINDOW, dispatched: AtomicBool::new(false) }
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.start_before {
            return Err(CollectionStop::TimedOut.error());
        }
        if self.dispatched.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return Err(CollectionStop::QueryLimit.error());
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl FleetBackend for AdmittedBackend<'_> {
    fn validate_target(&self, connection: &str) -> Result<()> {
        self.backend.validate_target(connection)
    }
    async fn discover(&self, connection: &str, request: &IntervalDiscoveryRequest) -> Result<IntervalDiscoveryPage> {
        self.check()?;
        self.backend.discover(connection, request).await
    }
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        self.check()?;
        self.backend.capture(connection, request, outbox, cancellation).await
    }
}
