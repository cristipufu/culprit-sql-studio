//! Recovery selection is independent of warehouse selection. Native providers own configuration and authentication.
use super::config::RecoveryConfig;
use culprit_auth::CredentialResolver;
use culprit_collector::delivery::object_store::provider::{RecoveryProvider, RecoveryTarget};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Default)]
pub(super) struct RecoveryRegistry(BTreeMap<String, Arc<dyn RecoveryProvider>>);
impl RecoveryRegistry {
    pub fn builtins(providers: Vec<Arc<dyn RecoveryProvider>>) -> Self {
        Self(providers.into_iter().map(|provider| (provider.name().to_string(), provider)).collect())
    }

    pub fn register(&mut self, provider: Arc<dyn RecoveryProvider>) -> Result<()> {
        let name = provider.name();
        if name.is_empty()
            || name.len() > 64
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.0.contains_key(name)
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        self.0.insert(name.to_string(), provider);
        Ok(())
    }
    pub fn configure(&self, config: &RecoveryConfig, credentials: &dyn CredentialResolver) -> Result<RecoveryTarget> {
        self.0.get(&config.provider).ok_or_else(|| DeliveryProblem::InvalidContract.error())?.configure(
            &config.options,
            &config.credential,
            credentials,
        )
    }
}
