//! Composition root for opt-in adapters. Runtime registration uses the same contract as the built-ins.
use super::*;
use culprit_collector::delivery::object_store::provider::RecoveryProvider;

pub struct CollectorRuntime {
    pub(super) recovery: recovery::RecoveryRegistry,
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
            credentials: None,
            evidence_stores: evidence::stores::StoreRegistry::builtins(),
        }
    }
}
impl CollectorRuntime {
    pub fn empty() -> Self {
        Self {
            recovery: recovery::RecoveryRegistry::default(),
            credentials: None,
            evidence_stores: evidence::stores::StoreRegistry::default(),
        }
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
