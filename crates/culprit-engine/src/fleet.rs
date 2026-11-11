//! Background execution is an adapter over the existing workspace lifecycle, never a second provider stack.
use crate::Workspace;
use culprit_collector::{capture::CollectionCancellation, fleet::FleetBackend, outbox::Outbox};
use culprit_core::{AuthKind, Error, Result, telemetry::*};

#[async_trait::async_trait]
impl FleetBackend for Workspace {
    fn validate_target(&self, connection: &str) -> Result<()> {
        let stored = self.store.get(connection)?;
        let unattended = match stored.auth {
            AuthKind::Entra => self.azure_credential.is_some(),
            AuthKind::Password => stored.password_env.is_some(),
            AuthKind::AwsIam => false,
            AuthKind::None => true,
        };
        if !unattended {
            return Err(Error::invalid(culprit_core::t!("fleet.unattended_auth")));
        }
        Ok(())
    }
    async fn discover(&self, connection: &str, request: &IntervalDiscoveryRequest) -> Result<IntervalDiscoveryPage> {
        self.validate_target(connection)?;
        self.discover_intervals(connection, request).await
    }
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        self.validate_target(connection)?;
        self.collect_into_outbox(connection, request, outbox, cancellation).await
    }
}
