use super::*;
use culprit_auth::{BearerTokenProvider, CredentialResolver, DatabaseTokenProvider, PasswordCredential};
use std::sync::Mutex;
use zeroize::Zeroizing;

struct NoAcquisition;
#[async_trait::async_trait]
impl BearerTokenProvider for NoAcquisition {
    async fn bearer_token(&self, _: &str) -> Result<Zeroizing<String>> {
        panic!("offline validation acquired a bearer token")
    }
}
#[async_trait::async_trait]
impl DatabaseTokenProvider for NoAcquisition {
    fn auth_kind(&self) -> culprit_core::AuthKind {
        culprit_core::AuthKind::Entra
    }
    async fn database_token(
        &self,
        _: culprit_auth::DatabaseTokenRequest<'_>,
    ) -> Result<culprit_auth::DatabaseCredential> {
        panic!("offline validation acquired a database token")
    }
}
#[derive(Default)]
struct Profiles(Mutex<Vec<String>>);
impl CredentialResolver for Profiles {
    fn bearer(&self, profile: &str) -> Result<Arc<dyn BearerTokenProvider>> {
        self.0.lock().unwrap().push(format!("bearer:{profile}"));
        Ok(Arc::new(NoAcquisition))
    }
    fn database(&self, profile: &str) -> Result<Arc<dyn DatabaseTokenProvider>> {
        self.0.lock().unwrap().push(format!("database:{profile}"));
        Ok(Arc::new(NoAcquisition))
    }
    fn validate_password(&self, _: &str) -> Result<()> {
        panic!("not a password fixture")
    }
    fn password(&self, _: &str) -> Result<PasswordCredential> {
        panic!("offline password read")
    }
}

#[test]
fn generic_resolver_receives_each_roles_own_profile_without_acquiring_credentials() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let TargetConfig::Adx { delivery, .. } = &mut config.target else { panic!("ADX fixture") };
    delivery.credential = "staging".into();
    let profiles = Arc::new(Profiles::default());
    CollectorRuntime::default().with_credentials(profiles.clone()).validate(&config).unwrap();
    assert_eq!(
        *profiles.0.lock().unwrap(),
        ["database:source", "bearer:recovery", "bearer:analytics", "bearer:staging"]
    );
    profiles.0.lock().unwrap().clear();
    ConfiguredAdapters::new(
        &config,
        SourceMode::DeliveryOnly,
        &CollectorRuntime::default().with_credentials(profiles.clone()),
    )
    .unwrap();
    assert_eq!(*profiles.0.lock().unwrap(), ["bearer:recovery", "bearer:analytics", "bearer:staging"]);
    assert!(!config.state.exists());
    assert!(!config.workspace.join("collection-admission").exists());
}

#[test]
fn queued_ingestion_resolves_its_own_identity_without_resolving_a_staging_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let TargetConfig::Adx { delivery, .. } = &mut config.target else { panic!("ADX fixture") };
    delivery.credential = "ingestion".into();
    delivery.transport = AdxDeliveryTransport::default();
    let profiles = Arc::new(Profiles::default());
    CollectorRuntime::default().with_credentials(profiles.clone()).validate(&config).unwrap();
    assert_eq!(
        *profiles.0.lock().unwrap(),
        ["database:source", "bearer:recovery", "bearer:analytics", "bearer:ingestion"]
    );
    assert!(!config.state.exists());
}
