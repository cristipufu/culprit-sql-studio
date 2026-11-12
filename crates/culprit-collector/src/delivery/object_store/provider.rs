//! Runtime-pluggable recovery adapters. Configuration creates credential owners without acquiring secrets or tokens.
use super::transport::ObjectTransport;
use culprit_auth::CredentialResolver;
use culprit_core::Result;
use serde_json::Value;
use std::sync::Arc;

pub struct RecoveryTarget {
    pub transport: Arc<dyn ObjectTransport>,
    /// The access boundary (bucket or container), excluding a prefix. Staging must not expose private recovery data.
    pub access_scope: String,
}

pub trait RecoveryProvider: Send + Sync {
    fn name(&self) -> &str;
    fn configure(
        &self,
        options: &Value,
        credential: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<RecoveryTarget>;
}
