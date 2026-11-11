//! Workspace authentication composes with the source-independent capture controller.
use crate::Workspace;
use culprit_collector::{
    admission::SourceAdmission,
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::Outbox,
};
use culprit_core::{
    Result,
    telemetry::{CollectionManifest, CollectionRequest, TelemetrySource},
};
use std::path::Path;

struct AuthenticatedSource<'a> {
    workspace: &'a Workspace,
    name: &'a str,
    admission: &'a SourceAdmission,
    observation_id: &'a str,
}
#[async_trait::async_trait]
impl SourceFactory for AuthenticatedSource<'_> {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.admission.started(self.observation_id.to_owned(), self.name.to_owned()).await?;
        self.workspace.connection_service().provider(self.name).await?.telemetry_source(request)
    }
}
impl Workspace {
    pub async fn discover_intervals(
        &self,
        name: &str,
        request: &culprit_core::telemetry::IntervalDiscoveryRequest,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        request.validate()?;
        let observed = self.connection_service().observed_provider(name).await?;
        let result = observed.provider().telemetry_intervals(request).await.and_then(|page| {
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
        self.connection(name)?;
        let admission = SourceAdmission::acquire(self.collection_admission_path()).await?;
        let identity = capture::observation_identity()?;
        let source = AuthenticatedSource {
            workspace: self,
            name,
            admission: &admission,
            observation_id: &identity.observation_id,
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
        request.validate()?;
        self.connection(name)?;
        let admission = SourceAdmission::acquire(self.collection_admission_path()).await?;
        let mut lease = outbox.reserve(name, &request).await?;
        let directory = lease.directory().to_owned();
        let identity = lease.identity().clone();
        let source = AuthenticatedSource {
            workspace: self,
            name,
            admission: &admission,
            observation_id: &identity.observation_id,
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
        self.settle_collection(&admission).await?;
        lease.finish().await?;
        result
    }
}
