use super::*;
use culprit_auth::{BearerTokenProvider, CredentialResolver, DatabaseTokenProvider, PasswordCredential};
use culprit_collector::{
    analytical::test_support::Blobs,
    delivery::object_store::provider::{RecoveryProvider, RecoveryTarget},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Mutex};
use zeroize::Zeroizing;

struct NoAcquisition;
#[async_trait::async_trait]
impl BearerTokenProvider for NoAcquisition {
    async fn bearer_token(&self, _: &str) -> Result<Zeroizing<String>> {
        panic!("constructing a read-only client must not acquire a token")
    }
}
#[derive(Default)]
pub(super) struct Profiles(pub(super) Mutex<Vec<String>>);
impl CredentialResolver for Profiles {
    fn bearer(&self, profile: &str) -> Result<Arc<dyn BearerTokenProvider>> {
        self.0.lock().unwrap().push(format!("bearer:{profile}"));
        Ok(Arc::new(NoAcquisition))
    }
    fn database(&self, _: &str) -> Result<Arc<dyn DatabaseTokenProvider>> {
        panic!("archive investigation must never resolve a database credential")
    }
    fn validate_password(&self, profile: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("validate-password:{profile}"));
        Ok(())
    }
    fn password(&self, profile: &str) -> Result<PasswordCredential> {
        self.0.lock().unwrap().push(format!("password:{profile}"));
        Ok(PasswordCredential {
            username: "query-reader".into(),
            password: Zeroizing::new("private-password-canary".into()),
        })
    }
}
struct Recovery {
    objects: Arc<Blobs>,
}
impl RecoveryProvider for Recovery {
    fn name(&self) -> &str {
        "fixture-recovery"
    }
    fn configure(
        &self,
        _: &serde_json::Value,
        profile: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<RecoveryTarget> {
        credentials.bearer(profile)?;
        Ok(RecoveryTarget { transport: self.objects.clone(), access_scope: "private-fixture".into() })
    }
}
fn config(provider: &str, options: serde_json::Value) -> EvidenceConfig {
    EvidenceConfig {
        pipeline: "pipeline".into(),
        shard: "alpha".into(),
        credentials: BTreeMap::new(),
        recovery: super::super::super::config::RecoveryConfig {
            provider: "fixture-recovery".into(),
            options: json!({}),
            credential: "archive-reader".into(),
        },
        warehouse: EvidenceStoreConfig { provider: provider.into(), options, credential: "warehouse-reader".into() },
    }
}

#[test]
fn both_native_read_factories_resolve_only_independent_recovery_and_query_profiles() {
    let profiles = Arc::new(Profiles::default());
    let objects = Arc::new(Blobs::default());
    let runtime = CollectorRuntime::default()
        .with_credentials(profiles.clone())
        .with_recovery_provider(Arc::new(Recovery { objects: objects.clone() }))
        .unwrap();
    #[cfg(feature = "store-adx")]
    {
        let config = config(
            "adx",
            json!({"endpoint":"https://never-contact.westeurope.kusto.windows.net","database":"archive"}),
        );
        let service = runtime.evidence(&config).unwrap();
        assert!(service.contract.destination().starts_with("adx:"));
        assert_eq!(*profiles.0.lock().unwrap(), ["bearer:archive-reader", "bearer:warehouse-reader"]);
        profiles.0.lock().unwrap().clear();
    }
    #[cfg(feature = "store-clickhouse")]
    {
        let config = config("clickhouse", json!({"endpoint":"https://never-contact.invalid","database":"archive"}));
        let service = runtime.evidence(&config).unwrap();
        assert!(service.contract.destination().starts_with("clickhouse:"));
        assert_eq!(*profiles.0.lock().unwrap(), ["bearer:archive-reader", "password:warehouse-reader"]);
    }
    assert!(objects.objects.lock().unwrap().is_empty(), "configuration must not initialize remote history");
    assert!(objects.uploads.lock().unwrap().is_empty());
}

#[test]
fn query_only_configuration_does_not_validate_unused_source_or_delivery_profiles() {
    use crate::credentials::CredentialProfile;
    let mut config = config("unused", json!({}));
    for name in ["archive-reader", "warehouse-reader"] {
        config.credentials.insert(
            name.into(),
            CredentialProfile::PasswordEnvironment {
                username: "reader".into(),
                variable: "CULPRIT_ARCHIVE_TEST_UNUSED_ENV".into(),
            },
        );
    }
    config.credentials.insert(
        "unused-source".into(),
        CredentialProfile::PasswordEnvironment {
            username: String::new(),
            variable: "private-invalid-source-canary".into(),
        },
    );
    assert!(config.credentials().is_ok());
    config.warehouse.credential = "missing-profile".into();
    assert!(config.credentials().is_err());
}

#[test]
fn archive_configuration_and_adapter_registration_refuse_unknown_or_oversized_inputs_without_echoing_them() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("archive.json");
    let value = serde_json::to_value(config("adx", json!({}))).unwrap();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(EvidenceConfig::from_file(&path).is_ok());
    let mut unknown = value;
    unknown["workspace"] = json!("private-workspace-canary");
    std::fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    let error = EvidenceConfig::from_file(&path).err().unwrap();
    assert!(!error.to_string().contains("canary"));
    std::fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(EvidenceConfig::from_file(&path).is_err());
    let registry = stores::StoreRegistry::builtins();
    let config = EvidenceStoreConfig {
        provider: "unregistered".into(),
        options: json!({"secret":"private-canary"}),
        credential: "unknown".into(),
    };
    let error = registry.configure(&config, &Profiles::default()).err().unwrap();
    assert!(!error.to_string().contains("canary"));
}
