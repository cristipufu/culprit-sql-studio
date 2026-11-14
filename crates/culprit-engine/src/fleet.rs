//! Background execution is an adapter over the existing workspace lifecycle, never a second provider stack.
use crate::Workspace;
use culprit_collector::{
    capture::CollectionCancellation,
    fleet::{FleetBackend, FleetCaptureOutcome, FleetSourceAdmission},
    outbox::Outbox,
};
use culprit_core::{AuthKind, Error, Result, telemetry::*};

#[async_trait::async_trait]
impl FleetBackend for Workspace {
    fn route_revision(&self, connection: &str) -> Result<Option<String>> {
        self.connection_service().route_revision(connection).map(Some)
    }
    fn telemetry_recipes(&self, connection: &str) -> Result<Vec<RecipeDescriptor>> {
        Workspace::telemetry_recipes(self, connection)
    }
    fn qualification_component(&self, connection: &str) -> Result<Option<culprit_core::ComponentVersion>> {
        self.telemetry_qualification_component(connection)
    }
    fn verify_cost(
        &self,
        connection: &str,
        receipt: &RecipeCostReceipt,
        source: &CollectionDescription,
        record: &ExportedRecord,
    ) -> Result<bool> {
        self.verify_telemetry_cost(connection, receipt, source, record)
    }
    fn validate_target(&self, connection: &str) -> Result<()> {
        let stored = self.store.get(connection)?;
        let unattended = match stored.auth {
            AuthKind::Entra | AuthKind::AwsIam => {
                self.tokens_for(connection).is_some_and(|provider| provider.auth_kind() == stored.auth)
            }
            AuthKind::Password => stored.password_env.is_some(),
            AuthKind::None => true,
        };
        if !unattended {
            return Err(Error::invalid(culprit_core::t!("fleet.unattended_auth")));
        }
        Ok(())
    }
    async fn discover(
        &self,
        connection: &str,
        request: &IntervalDiscoveryRequest,
        admission: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage> {
        admission.window.check_start()?;
        self.validate_target(connection)?;
        self.discover_intervals_admitted(connection, request, &admission).await
    }
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admission: FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        let validation = admission.window.check_start().and_then(|()| self.validate_target(connection));
        if let Err(error) = validation {
            return FleetCaptureOutcome { result: Err(error), source_work: SourceWorkState::NotStarted, attempt: None };
        }
        self.collect_into_outbox_admitted(connection, request, outbox, cancellation, &admission).await
    }
}
