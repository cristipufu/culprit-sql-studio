//! Provider selection and lifecycle dispatch; no native token, tenant or sign-in policy lives here.
use culprit_auth::{AuthenticationService, DatabaseAuthentication, DatabaseCredential, SecretStore, SignInHandler};
use culprit_core::{ConnectionSpec, Error, Result, t};
use std::sync::Arc;

pub(crate) struct AuthenticationRegistry(Vec<Arc<dyn DatabaseAuthentication>>);
impl AuthenticationRegistry {
    pub(super) fn builtins(providers: Vec<Arc<dyn DatabaseAuthentication>>) -> Self {
        Self(providers)
    }
    pub fn register(&mut self, provider: Arc<dyn DatabaseAuthentication>) {
        self.0.retain(|existing| existing.auth_kind() != provider.auth_kind());
        self.0.push(provider);
    }
}
#[async_trait::async_trait]
impl AuthenticationService for AuthenticationRegistry {
    async fn credential(&self, connection: &ConnectionSpec, secrets: &dyn SecretStore) -> Result<DatabaseCredential> {
        self.0
            .iter()
            .find(|provider| provider.auth_kind() == connection.auth)
            .ok_or_else(|| Error::invalid(t!("engine.credentials.invalid_profile")))?
            .credential(connection, secrets)
            .await
    }
    fn set_sign_in_handler(&self, handler: Option<SignInHandler>) {
        for provider in &self.0 {
            provider.set_sign_in_handler(handler.clone());
        }
    }
    fn cancel_sign_in(&self) {
        for provider in &self.0 {
            provider.cancel_sign_in();
        }
    }
    fn sign_out(&self, connections: &[ConnectionSpec], secrets: &dyn SecretStore) -> Result<usize> {
        let mut removed = 0;
        for provider in &self.0 {
            removed += provider.sign_out(connections, secrets)?;
        }
        Ok(removed)
    }
}
