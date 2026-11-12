//! Workspace-wide native ownership, shared by scans and the capture controller.
use crate::{connection::Connections, store::Paths};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use culprit_read::admission::{SourceAdmission, SourcePermit};

pub(crate) struct SourceOperation<'a> {
    journal: SourceAdmission,
    permit: SourcePermit<'a>,
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
    pub(crate) async fn observed_provider(&self, name: &str) -> Result<ObservedProvider<'a>> {
        let operation = self.source_operation(name).await?;
        match self.provider(name).await {
            Ok(provider) => Ok(ObservedProvider { provider, operation }),
            Err(error) => operation.finish(Err(error), SourceShutdown::Quiescent).await,
        }
    }
    async fn source_operation(&self, name: &str) -> Result<SourceOperation<'a>> {
        self.connection(name)?;
        let journal = SourceAdmission::acquire(admission_path(self.paths)).await?;
        let mut permit = self.source_gate.acquire().await?;
        journal.started(culprit_read::admission::observation_identity()?.observation_id, name.to_owned()).await?;
        permit.begin()?;
        Ok(SourceOperation { journal, permit })
    }
}
impl SourceOperation<'_> {
    pub async fn finish<T>(mut self, result: Result<T>, shutdown: SourceShutdown) -> Result<T> {
        let uncertain =
            shutdown == SourceShutdown::Unconfirmed || result.as_ref().err().is_some_and(native_shutdown_uncertain);
        let shutdown = if uncertain { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent };
        self.permit.settle(shutdown)?;
        if uncertain {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        self.journal.confirmed_quiet().await?;
        result
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
