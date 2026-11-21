//! Composition root for opt-in adapters. Runtime registration uses the same contract as the built-ins.
use super::*;
use culprit_collector::delivery::object_store::provider::RecoveryProvider;

pub struct CollectorRuntime {
    pub(super) recovery: recovery::RecoveryRegistry,
    pub(super) providers: Vec<(culprit_core::Engine, crate::ProviderRegistration)>,
    pub(super) credentials: Option<Arc<dyn culprit_auth::CredentialResolver>>,
    pub(super) evidence_stores: evidence::stores::StoreRegistry,
}
impl Default for CollectorRuntime {
    fn default() -> Self {
        let providers: Vec<Arc<dyn RecoveryProvider>> = vec![
            #[cfg(feature = "recovery-azure")]
            Arc::new(culprit_blob_azure::AzureRecoveryProvider),
            #[cfg(feature = "recovery-s3")]
            Arc::new(culprit_blob_s3::S3RecoveryProvider),
        ];
        Self {
            recovery: recovery::RecoveryRegistry::builtins(providers),
            providers: vec![],
            credentials: None,
            evidence_stores: evidence::stores::StoreRegistry::builtins(),
        }
    }
}
impl CollectorRuntime {
    pub fn empty() -> Self {
        Self {
            recovery: recovery::RecoveryRegistry::default(),
            providers: vec![],
            credentials: None,
            evidence_stores: evidence::stores::StoreRegistry::default(),
        }
    }
    /// Select the same trusted provider registration used by Workspace; configuration files cannot supply it.
    pub fn with_provider(mut self, engine: culprit_core::Engine, registration: crate::ProviderRegistration) -> Self {
        self.providers.retain(|(selected, _)| *selected != engine);
        self.providers.push((engine, registration));
        self
    }
    pub(super) fn workspace(&self, path: &Path) -> Workspace {
        self.providers.iter().fold(Workspace::open_in(path), |workspace, (engine, registration)| {
            workspace.with_provider(*engine, registration.clone())
        })
    }
    pub fn with_recovery_provider(mut self, provider: Arc<dyn RecoveryProvider>) -> Result<Self> {
        self.recovery.register(provider)?;
        Ok(self)
    }
    /// Embedders may supply their own profile registry. No role inherits a different profile implicitly.
    pub fn with_credentials(mut self, credentials: Arc<dyn culprit_auth::CredentialResolver>) -> Self {
        self.credentials = Some(credentials);
        self
    }
    pub fn validate(&self, config: &CollectorConfig) -> Result<()> {
        ConfiguredAdapters::new(config, SourceMode::Collect, self).map(|_| ())
    }
    pub async fn initialize(&self, config: &CollectorConfig) -> Result<()> {
        Adapters::open(config, true, SourceMode::Collect, self).await.map(|_| ())
    }
    pub async fn run(
        &self,
        config: &CollectorConfig,
        mode: SourceMode,
        cancellation: &CollectionCancellation,
        max_work: Option<u32>,
        reporter: &dyn ServiceReporter,
    ) -> Result<FleetSummary> {
        run_with_runtime(self, config, mode, cancellation, max_work, reporter).await
    }
}
