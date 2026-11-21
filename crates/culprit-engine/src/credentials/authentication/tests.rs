use super::*;
use crate::{NewConnection, ProviderConstructionPolicy, ProviderRegistration, Workspace};
use culprit_core::{AuthKind, Engine};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn builtin_registration_matches_compiled_capabilities_without_acquiring_credentials() {
    let registry = crate::credentials::authentication();
    let kinds = registry.0.iter().map(|provider| provider.auth_kind()).collect::<Vec<_>>();
    assert_eq!(kinds.contains(&AuthKind::Entra), cfg!(feature = "auth-azure"));
    assert_eq!(kinds.contains(&AuthKind::AwsIam), cfg!(feature = "auth-aws"));
    assert_eq!(kinds.len(), usize::from(cfg!(feature = "auth-azure")) + usize::from(cfg!(feature = "auth-aws")));
}
struct UntouchedSecrets;
impl SecretStore for UntouchedSecrets {
    fn get(&self, _: &str) -> Result<Option<String>> {
        panic!("unregistered authentication must not read a secret")
    }
    fn set(&self, _: &str, _: &str) -> Result<()> {
        panic!("unregistered authentication must not write a secret")
    }
    fn forget(&self, _: &str) {
        panic!("unregistered authentication must not retire another identity")
    }
    fn delete(&self, _: &str) -> Result<()> {
        panic!("unregistered authentication must not delete a secret")
    }
}
#[tokio::test]
async fn an_unregistered_method_refuses_before_secret_or_adapter_work() {
    let registry = AuthenticationRegistry::builtins(vec![]);
    for auth in [AuthKind::Entra, AuthKind::AwsIam] {
        let spec = ConnectionSpec { auth, ..Default::default() };
        let error = registry.credential(&spec, &UntouchedSecrets).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    workspace: Workspace,
    factories: Arc<AtomicUsize>,
    certificate: Arc<Mutex<Option<String>>>,
}
fn fixture(auth: AuthKind, certificate: Option<&str>) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let factories = Arc::new(AtomicUsize::new(0));
    let observed = factories.clone();
    let selected_certificate = Arc::new(Mutex::new(None));
    let captured_certificate = selected_certificate.clone();
    let workspace = Workspace::open_in(directory.path()).with_secrets(Arc::new(UntouchedSecrets)).with_provider(
        Engine::Postgres,
        ProviderRegistration::new(
            Arc::new(move |spec, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                *captured_certificate.lock().unwrap() = spec.option("sslrootcert").map(str::to_string);
                Err(Error::internal("owned provider boundary"))
            }),
            ProviderConstructionPolicy::DeferredNativeWork,
            vec![],
        )
        .unwrap(),
    );
    workspace
        .add_connection(NewConnection {
            name: "fixture".into(),
            input: "postgres://app@fixture.abc.eu-central-1.rds.amazonaws.com/sample".into(),
            engine: Some(Engine::Postgres),
            auth: Some(auth),
            ..Default::default()
        })
        .unwrap();
    if let Some(certificate) = certificate {
        let mut stored = workspace.store.get("fixture").unwrap();
        stored.options.insert("sslrootcert".into(), certificate.into());
        workspace.store.put("fixture", stored).unwrap();
    }
    Fixture { directory, workspace, factories, certificate: selected_certificate }
}
#[cfg(any(not(feature = "auth-azure"), not(feature = "auth-aws")))]
async fn require_disabled(auth: AuthKind) {
    let fixture = fixture(auth, None);
    let workspace = &fixture.workspace;
    // A regression must fail here before a native adapter could acquire credentials.
    assert!(!workspace.authentication.0.iter().any(|provider| provider.auth_kind() == auth));
    let error = workspace.connection_service().provider("fixture").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 0);
}
#[cfg(not(feature = "auth-azure"))]
#[tokio::test]
async fn disabled_entra_refuses_before_native_factory_work() {
    require_disabled(AuthKind::Entra).await;
}
#[cfg(not(feature = "auth-aws"))]
#[tokio::test]
async fn disabled_iam_refuses_before_native_factory_work() {
    require_disabled(AuthKind::AwsIam).await;
}
#[tokio::test]
async fn an_explicit_certificate_file_does_not_require_a_builtin_adapter() {
    let fixture = fixture(AuthKind::None, Some("/owned-explicit-ca.pem"));
    let error = fixture.workspace.connection_service().provider("fixture").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.certificate.lock().unwrap().as_deref(), Some("/owned-explicit-ca.pem"));
    assert!(!fixture.directory.path().join("certs").exists());
}
#[cfg(feature = "auth-aws")]
#[tokio::test]
async fn enabled_rds_certificates_use_the_exact_owned_bundle() {
    let fixture = fixture(AuthKind::None, Some(culprit_core::cloud::aws::RDS_CA));
    let error = fixture.workspace.connection_service().provider("fixture").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 1);
    let path = fixture.certificate.lock().unwrap().clone().unwrap();
    assert_eq!(std::path::Path::new(&path), fixture.directory.path().join("certs/rds-global-bundle.pem"));
    assert_eq!(std::fs::read(path).unwrap(), culprit_auth_aws::rds_ca_bundle());
}
#[cfg(not(feature = "auth-aws"))]
#[tokio::test]
async fn unavailable_rds_bundle_refuses_before_even_an_injected_credential_or_native_factory() {
    struct Tokens(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl culprit_auth::DatabaseTokenProvider for Tokens {
        fn auth_kind(&self) -> AuthKind {
            AuthKind::AwsIam
        }
        async fn database_token(&self, _: culprit_auth::DatabaseTokenRequest<'_>) -> Result<DatabaseCredential> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(Error::internal("credential acquisition must not start"))
        }
    }
    let fixture = fixture(AuthKind::AwsIam, Some(culprit_core::cloud::aws::RDS_CA));
    let acquisitions = Arc::new(AtomicUsize::new(0));
    let workspace = fixture.workspace.with_database_tokens(Arc::new(Tokens(acquisitions.clone())));
    let error = workspace.connection_service().provider("fixture").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.factories.load(Ordering::SeqCst), 0);
    assert!(!fixture.directory.path().join("certs").exists());
    assert_eq!(
        workspace.store.get("fixture").unwrap().spec().option("sslrootcert"),
        Some(culprit_core::cloud::aws::RDS_CA)
    );
}
