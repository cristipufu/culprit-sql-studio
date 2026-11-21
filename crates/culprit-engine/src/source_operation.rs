//! Workspace-wide native ownership, shared by scans and the capture controller.
use crate::{connection::Connections, store::Paths};
use culprit_core::{
    ProviderCall, ProviderOperationObservation, ProviderReadDisposition, Result,
    telemetry::{CollectionStop, SourceAdmissionRoute, SourceShutdown},
};
use culprit_read::admission::{SourceAdmission, SourcePermit};

pub(crate) struct SourceOperation<'a> {
    journal: SourceAdmission,
    permit: SourcePermit<'a>,
    observation: ProviderOperationObservation,
    native_started: bool,
}
pub(crate) fn admission_path(paths: &Paths) -> std::path::PathBuf {
    paths.data.join("collection-admission")
}

#[must_use]
pub(crate) struct ObservedProvider<'a> {
    provider: Box<dyn culprit_core::Provider>,
    operation: SourceOperation<'a>,
}
impl ObservedProvider<'_> {
    pub fn provider(&self) -> &dyn culprit_core::Provider {
        self.provider.as_ref()
    }
    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        self.operation.finish(result, self.provider.source_shutdown()).await
    }
}
impl<'a> Connections<'a> {
    pub(crate) async fn observed_provider_at_route(
        &self,
        route: &crate::connection::SavedRoute,
    ) -> Result<ObservedProvider<'a>> {
        let operation = self.source_operation(&route.name, None).await?;
        match self.provider_at_route(route).await {
            Ok(provider) => Ok(ObservedProvider { provider, operation }),
            Err(error) => operation.finish(Err(error), SourceShutdown::Quiescent).await,
        }
    }

    pub(crate) async fn observed_provider(&self, name: &str) -> Result<ObservedProvider<'a>> {
        let operation = self.source_operation(name, None).await?;
        match self.provider(name).await {
            Ok(provider) => Ok(ObservedProvider { provider, operation }),
            Err(error) => operation.finish(Err(error), SourceShutdown::Quiescent).await,
        }
    }
    #[cfg(feature = "collector")]
    pub(crate) async fn observed_provider_admitted(
        &self,
        name: &str,
        admission: &culprit_collector::fleet::FleetSourceAdmission,
    ) -> Result<ObservedProvider<'a>> {
        admission.window.check_start()?;
        let operation = self.source_operation(name, Some(admission.window.start_before())).await?;
        let result = match admission.window.check_start() {
            Ok(()) => self.provider_admitted(name, admission).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(provider) => Ok(ObservedProvider { provider, operation }),
            Err(error) => operation.finish(Err(error), SourceShutdown::Quiescent).await,
        }
    }
    async fn source_operation(
        &self,
        name: &str,
        admitted_start_before: Option<tokio::time::Instant>,
    ) -> Result<SourceOperation<'a>> {
        self.source_operation_at(
            SourceAdmissionRoute::SavedConnection { name: name.into() },
            admitted_start_before,
            ProviderOperationObservation::unobserved(),
        )
        .await
    }
    pub(crate) async fn source_operation_at(
        &self,
        route: SourceAdmissionRoute,
        admitted_start_before: Option<tokio::time::Instant>,
        observation: ProviderOperationObservation,
    ) -> Result<SourceOperation<'a>> {
        if !route.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        if let Some(name) = route.saved_connection() {
            self.connection(name)?;
        }
        observation.require_open()?;
        observation.bind_route(route.clone());
        let journal = SourceAdmission::acquire(admission_path(self.paths)).await?;
        let mut permit = match admitted_start_before {
            Some(deadline) => tokio::time::timeout_at(deadline, self.source_gate.acquire())
                .await
                .map_err(|_| CollectionStop::TimedOut.error())??,
            None => self.source_gate.acquire().await?,
        };
        if admitted_start_before.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(CollectionStop::TimedOut.error());
        }
        observation.require_open()?;
        let id = match observation.observation_id() {
            Some(id) => id,
            None => culprit_read::admission::observation_identity()?.observation_id,
        };
        journal.started(id, route.clone()).await?;
        permit.begin()?;
        observation.admitted();
        Ok(SourceOperation { journal, permit, observation, native_started: false })
    }
}
impl SourceOperation<'_> {
    pub(crate) fn observation(&self) -> &ProviderOperationObservation {
        &self.observation
    }
    pub(crate) fn native_started(&mut self) {
        self.native_started = true;
        self.observation.native_started();
    }
    pub async fn finish<T>(self, result: Result<T>, shutdown: SourceShutdown) -> Result<T> {
        self.finish_call(result, shutdown).await.into_result()
    }
    pub(crate) async fn finish_call<T>(mut self, result: Result<T>, shutdown: SourceShutdown) -> ProviderCall<T> {
        let uncertain =
            shutdown == SourceShutdown::Unconfirmed || result.as_ref().err().is_some_and(native_shutdown_uncertain);
        let shutdown = if uncertain { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent };
        let (settlement, finalization) = self.settle(shutdown).await;
        let disposition = if settlement == SourceShutdown::Unconfirmed {
            ProviderReadDisposition::Unconfirmed
        } else if self.native_started {
            ProviderReadDisposition::Quiescent
        } else {
            ProviderReadDisposition::NotStarted
        };
        let acquisition = self.observation.source_finished(&result, disposition, finalization.clone());
        ProviderCall::completed(result, finalization, acquisition)
    }
    async fn settle(&mut self, shutdown: SourceShutdown) -> (SourceShutdown, Result<()>) {
        if let Err(error) = self.permit.settle(shutdown) {
            return (SourceShutdown::Unconfirmed, Err(error));
        }
        if shutdown == SourceShutdown::Unconfirmed {
            return (shutdown, Err(CollectionStop::CancellationUnconfirmed.error()));
        }
        (shutdown, self.journal.confirmed_quiet().await)
    }
}

#[cfg(all(test, feature = "collector"))]
mod tests;

/// Untyped outer timeouts retire a provider future without proving its native shutdown. A typed read timeout can
/// retain its native completion proof; the provider's aggregate state is checked separately before clearing debt.
pub(crate) fn native_shutdown_uncertain(error: &culprit_core::Error) -> bool {
    CollectionStop::from_error(error) == CollectionStop::CancellationUnconfirmed
        || (error.code == culprit_core::ErrorCode::Timeout
            && !matches!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::TimedOut))))
}
