//! Workspace authentication composes with the source-independent capture controller.
#[cfg(test)]
mod tests;
use crate::Workspace;
use culprit_collector::{
    capture::{self, CollectionCancellation, SourceFactory},
    fleet::{FleetCaptureOutcome, FleetSourceAdmission},
    outbox::Outbox,
};
use culprit_core::{
    Result,
    telemetry::{
        CollectionManifest, CollectionRequest, CollectionSelection, CollectionStop, DiagnosticAttemptReceipt,
        DiagnosticInvestigationLineage, TelemetryRecord, TelemetrySource,
    },
};
use culprit_read::admission::SourceAdmission;
use std::path::Path;

struct AuthenticatedSource<'a> {
    workspace: &'a Workspace,
    name: &'a str,
    admission: &'a SourceAdmission,
    observation_id: &'a str,
    admitted: Option<&'a FleetSourceAdmission>,
}
#[async_trait::async_trait]
impl SourceFactory for AuthenticatedSource<'_> {
    fn admission_window(&self) -> Option<capture::CollectionAdmissionWindow> {
        self.admitted.map(|admitted| admitted.window)
    }
    fn investigation_intent(&self) -> Option<&culprit_core::telemetry::DiagnosticInvestigationIntent> {
        self.admitted.and_then(|admitted| admitted.investigation.as_ref())
    }
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        if let Some(admitted) = self.admitted {
            admitted.window.check_start()?;
        }
        self.admission.started(self.observation_id.to_owned(), self.name.to_owned()).await?;
        let connections = self.workspace.connection_service();
        let provider = match self.admitted {
            Some(admitted) => connections.provider_admitted(self.name, admitted).await?,
            None => connections.provider(self.name).await?,
        };
        if let Some(admitted) = self.admitted {
            admitted.window.check_start()?;
        }
        provider.telemetry_source(request)
    }
}
impl Workspace {
    pub async fn discover_resources(
        &self,
        name: &str,
        request: &culprit_core::telemetry::ResourceDiscoveryRequest,
    ) -> Result<Vec<culprit_core::telemetry::CollectionResource>> {
        request.budget.validate()?;
        let observed = self.connection_service().observed_provider(name).await?;
        let result = observed.provider().telemetry_resources(request).await.and_then(|resources| {
            request.validate_result(&resources)?;
            if resources.iter().any(|resource| resource.identity.engine != observed.provider().engine()) {
                return Err(culprit_core::telemetry::CollectionStop::InvalidPage.error());
            }
            Ok(resources)
        });
        observed.finish(result).await
    }

    pub async fn discover_intervals(
        &self,
        name: &str,
        request: &culprit_core::telemetry::IntervalDiscoveryRequest,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        self.discover_intervals_with_admission(name, request, None).await
    }
    pub(crate) async fn discover_intervals_admitted(
        &self,
        name: &str,
        request: &culprit_core::telemetry::IntervalDiscoveryRequest,
        admitted: &FleetSourceAdmission,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        self.discover_intervals_with_admission(name, request, Some(admitted)).await
    }
    async fn discover_intervals_with_admission(
        &self,
        name: &str,
        request: &culprit_core::telemetry::IntervalDiscoveryRequest,
        admitted: Option<&FleetSourceAdmission>,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        request.validate()?;
        let connections = self.connection_service();
        let observed = match admitted {
            Some(admitted) => connections.observed_provider_admitted(name, admitted).await?,
            None => connections.observed_provider(name).await?,
        };
        let result = match admitted {
            Some(admitted) => match admitted.window.check_start() {
                Err(error) => Err(error),
                Ok(()) => tokio::time::timeout_at(
                    admitted.window.settle_before(),
                    observed.provider().telemetry_intervals(request),
                )
                .await
                .unwrap_or_else(|_| Err(culprit_core::limits::timeout_error())),
            },
            None => observed.provider().telemetry_intervals(request).await,
        }
        .and_then(|page| {
            if page.matches_request(request) {
                Ok(page)
            } else {
                Err(culprit_core::telemetry::CollectionStop::InvalidPage.error())
            }
        });
        observed.finish(result).await
    }
    pub(crate) fn collection_admission_path(&self) -> std::path::PathBuf {
        crate::source_operation::admission_path(&self.paths)
    }
    async fn settle_collection(&self, admission: &SourceAdmission) -> Result<()> {
        // Returning with admission open means the controller either never started native work or confirmed closure.
        // Dropped callers and uncertain native shutdown poison this same controller semaphore and retain disk debt.
        if !self.source_gate.is_closed() {
            admission.confirmed_quiet().await?;
        }
        Ok(())
    }
    pub async fn collection_admission_status(&self) -> Result<culprit_core::telemetry::CollectionAdmissionStatus> {
        SourceAdmission::inspect(self.collection_admission_path()).await
    }
    pub async fn acknowledge_collection_quiescence(
        &self,
        observation_id: &str,
    ) -> Result<culprit_core::telemetry::CollectionAdmissionStatus> {
        SourceAdmission::acknowledge(self.collection_admission_path(), observation_id).await
    }
    pub async fn collect_once(
        &self,
        name: &str,
        request: CollectionRequest,
        output: &Path,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        request.validate()?;
        if matches!(request.selection, CollectionSelection::FocusedPageWait { .. }) {
            // Focus requires the immutable original receipt, which only the outbox/recovery path can resolve.
            return Err(CollectionStop::InvalidPage.error());
        }
        self.connection(name)?;
        let admission = SourceAdmission::acquire(self.collection_admission_path()).await?;
        let identity = culprit_read::admission::observation_identity()?;
        let source = AuthenticatedSource {
            workspace: self,
            name,
            admission: &admission,
            observation_id: &identity.observation_id,
            admitted: None,
        };
        let result =
            capture::collect(&self.source_gate, &source, request, output, identity.clone(), cancellation, None).await;
        self.settle_collection(&admission).await?;
        result
    }
    pub async fn collect_into_outbox(
        &self,
        name: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        self.collect_into_outbox_with_admission(name, request, outbox, cancellation, None).await.result
    }
    pub(crate) async fn collect_into_outbox_admitted(
        &self,
        name: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admitted: &FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        if let Err(error) = admitted.window.check_start() {
            return FleetCaptureOutcome {
                result: Err(error),
                source_work: culprit_core::telemetry::SourceWorkState::NotStarted,
                attempt: None,
            };
        }
        self.collect_into_outbox_with_admission(name, request, outbox, cancellation, Some(admitted)).await
    }
    async fn collect_into_outbox_with_admission(
        &self,
        name: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admitted: Option<&FleetSourceAdmission>,
    ) -> FleetCaptureOutcome {
        use culprit_core::telemetry::SourceWorkState;
        let mut source_work = SourceWorkState::NotStarted;
        let mut attempt = None;
        let result = async {
            request.validate()?;
            let investigation = admitted
                .and_then(|admitted| admitted.investigation.as_ref())
                .map(DiagnosticInvestigationLineage::from_intent)
                .transpose()?;
            if investigation.as_ref().is_some_and(|owner| !owner.matches_request(&request)) {
                return Err(CollectionStop::InvalidPage.error());
            }
            if let CollectionSelection::FocusedPageWait { focus } = &request.selection {
                let (source, row) = outbox.diagnostic_evidence(&focus.parent).await?;
                let TelemetryRecord::Activity(original) = &row.record else {
                    return Err(CollectionStop::InvalidPage.error());
                };
                if !focus.matches_parent(&source, original, &focus.parent) {
                    return Err(CollectionStop::InvalidPage.error());
                }
            }
            self.connection(name)?;
            let admission = SourceAdmission::acquire(self.collection_admission_path()).await?;
            let mut lease = outbox.reserve(name, &request).await?;
            let directory = lease.directory().to_owned();
            let identity = lease.identity().clone();
            attempt = Some(DiagnosticAttemptReceipt { capture: identity.clone(), manifest_sha256: None });
            let source = AuthenticatedSource {
                workspace: self,
                name,
                admission: &admission,
                observation_id: &identity.observation_id,
                admitted,
            };
            let result = capture::collect(
                &self.source_gate,
                &source,
                request,
                &directory,
                identity.clone(),
                cancellation,
                Some(&mut lease),
            )
            .await;
            // Observe settlement while this same durable source owner is still held. A new workspace's open gate
            // cannot prove an older journal debt quiet, and another operation cannot race this receipt.
            source_work =
                if self.source_gate.is_closed() { SourceWorkState::Unconfirmed } else { SourceWorkState::Quiescent };
            let settlement = self.settle_collection(&admission).await;
            let finished = lease.finish().await;
            // Even failed collection/storage leaves an allocated identity. Only the exact existing manifest can
            // add its digest; an invalid artifact must not erase that identity or native shutdown uncertainty.
            attempt = Some(outbox.attempt_receipt(&identity, investigation.as_ref()).await?);
            settlement?;
            finished?;
            result
        }
        .await;
        FleetCaptureOutcome { result, source_work, attempt }
    }
}
