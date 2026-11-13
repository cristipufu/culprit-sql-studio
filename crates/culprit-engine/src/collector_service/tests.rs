use super::*;
use crate::{NewConnection, credentials::CredentialProfile};
use culprit_auth_azure::IdentityConfig;
use culprit_blob_azure::AzureBlobConfig;
use culprit_core::{AuthKind, Engine, Environment};

fn config(root: &Path) -> CollectorConfig {
    let workspace = root.join("workspace");
    Workspace::open_in(&workspace)
        .add_connection(NewConnection {
            name: "fixture".into(),
            input: "Server=never-contact.invalid;Database=fixture;Authentication=Active Directory Default".into(),
            environment: Environment::Development,
            engine: Some(Engine::Mssql),
            database: None,
            user: None,
            password: None,
            password_env: None,
            auth: Some(AuthKind::Entra),
            security: None,
            ssh: None,
        })
        .unwrap();
    CollectorConfig {
        pipeline: "test-pipeline".into(),
        shard: "alpha".into(),
        workspace,
        state: root.join("runtime"),
        inventory: FleetInventory {
            targets: vec![FleetTarget {
                id: "fixture".into(),
                subject: culprit_core::telemetry::CollectionSubject::Database,
                connection: "fixture".into(),
                shard: "alpha".into(),
                admission_group: "pool".into(),
                schedules: vec![FleetSchedule {
                    id: "activity".into(),
                    every_seconds: 60,
                    jitter_seconds: 0,
                    collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                    budget: CollectionBudget::default(),
                }],
            }],
        },
        source_credentials: [("fixture".into(), "source".into())].into(),
        credentials: [
            ("source", None),
            ("recovery", Some("11111111-1111-1111-1111-111111111111")),
            ("analytics", Some("22222222-2222-2222-2222-222222222222")),
        ]
        .into_iter()
        .map(|(name, client_id)| {
            (
                name.into(),
                CredentialProfile::Azure {
                    identity: IdentityConfig::ManagedIdentity { client_id: client_id.map(str::to_string) },
                },
            )
        })
        .collect(),
        recovery: RecoveryConfig {
            provider: "azure-blob".into(),
            credential: "recovery".into(),
            options: serde_json::json!({"account":"nevercontact", "container":"recovery", "prefix":"test/alpha"}),
        },
        target: TargetConfig::Adx {
            credential: "analytics".into(),
            query: culprit_store_adx::client::AdxConfig {
                endpoint: "https://never-contact.westeurope.kusto.windows.net".into(),
                database: "test".into(),
            },
            delivery: AdxDeliveryConfig {
                credential: "analytics".into(),
                transport: AdxDeliveryTransport::EventGrid {
                    ready: AzureBlobConfig {
                        account: "nevercontact".into(),
                        container: "ready".into(),
                        prefix: "test/alpha".into(),
                    },
                },
            },
        },
        limits: OutboxLimits::default(),
        health_listen: "127.0.0.1:9098".parse().unwrap(),
    }
}

#[test]
fn offline_validation_configures_all_adapters_without_tokens_connections_or_runtime_files() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    validate(&config).unwrap();
    let adapters = ConfiguredAdapters::new(&config, SourceMode::Collect, &CollectorRuntime::default()).unwrap();
    let policy = adapters.destination.delivery_policy();
    assert_eq!(policy.visibility_delay_ms, 60_000);
    assert_eq!(policy.visibility_timeout_ms, Some(360_000));
    assert!(!config.state.exists());
    assert!(!config.workspace.join("collection-admission").exists());
    let path = root.path().join("collector.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    validate(&CollectorConfig::from_file(&path).unwrap()).unwrap();
    assert!(!config.state.exists());
}

#[test]
fn omitted_adx_transport_selects_queued_delivery_without_a_ready_container() {
    let root = tempfile::tempdir().unwrap();
    let mut value = serde_json::to_value(config(root.path())).unwrap();
    value["target"]["delivery"] = serde_json::json!({"credential":"analytics"});
    let config: CollectorConfig = serde_json::from_value(value.clone()).unwrap();
    let TargetConfig::Adx { delivery, .. } = &config.target else { panic!("ADX fixture") };
    assert!(matches!(delivery.transport, AdxDeliveryTransport::Queued { endpoint: None }));
    validate(&config).unwrap();
    assert!(!config.state.exists());
    value["target"]["delivery"]["transport"] =
        serde_json::json!({"kind":"queued","endpoint":"http://unsafe.kusto.windows.net"});
    let invalid: CollectorConfig = serde_json::from_value(value).unwrap();
    assert!(validate(&invalid).is_err());
}

struct NoObjectWork;
#[async_trait::async_trait]
impl culprit_collector::delivery::object_store::transport::ObjectTransport for NoObjectWork {
    async fn get(&self, _: &str) -> Result<Option<culprit_collector::delivery::object_store::transport::ObjectRead>> {
        panic!("offline object read")
    }
    async fn put(
        &self,
        _: &str,
        _: Vec<u8>,
        _: culprit_collector::delivery::object_store::transport::WriteCondition<'_>,
    ) -> Result<culprit_collector::delivery::object_store::transport::ObjectWrite> {
        panic!("offline object write")
    }
    async fn put_file(
        &self,
        _: &str,
        _: &Path,
        _: u64,
    ) -> Result<culprit_collector::delivery::object_store::transport::ObjectWrite> {
        panic!("offline object upload")
    }
}
struct ExternalRecovery;
impl culprit_collector::delivery::object_store::provider::RecoveryProvider for ExternalRecovery {
    fn name(&self) -> &str {
        "external"
    }
    fn configure(
        &self,
        options: &serde_json::Value,
        profile: &str,
        _: &dyn culprit_auth::CredentialResolver,
    ) -> Result<culprit_collector::delivery::object_store::provider::RecoveryTarget> {
        assert_eq!(profile, "external-profile");
        assert_eq!(options["namespace"], "foreign");
        Ok(culprit_collector::delivery::object_store::provider::RecoveryTarget {
            transport: Arc::new(NoObjectWork),
            access_scope: "external:foreign".into(),
        })
    }
}
#[test]
fn recovery_plugins_use_the_same_runtime_registration_without_builtin_configuration_or_io() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.recovery = RecoveryConfig {
        provider: "external".into(),
        options: serde_json::json!({"namespace":"foreign"}),
        credential: "external-profile".into(),
    };
    assert!(validate(&config).is_err());
    let runtime = CollectorRuntime::empty().with_recovery_provider(Arc::new(ExternalRecovery)).unwrap();
    runtime.validate(&config).unwrap();
    assert!(runtime.with_recovery_provider(Arc::new(ExternalRecovery)).is_err());
    assert!(!config.state.exists());
}

#[test]
fn one_fleet_can_bind_azure_and_aws_sources_to_distinct_credential_profiles_offline() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let workspace = Workspace::open_in(&config.workspace);
    workspace
        .add_connection(NewConnection {
            name: "rds".into(),
            input: "postgres://app@fixture.abc.eu-central-1.rds.amazonaws.com/shop".into(),
            engine: Some(Engine::Postgres),
            auth: Some(AuthKind::AwsIam),
            ..Default::default()
        })
        .unwrap();
    let mut target = config.inventory.targets[0].clone();
    target.id = "rds-database".into();
    target.connection = "rds".into();
    target.admission_group = "rds".into();
    config.inventory.targets.push(target);
    config.credentials.insert("aws-source".into(), CredentialProfile::Aws { profile: Some("fixture-aws".into()) });
    config.source_credentials.insert("rds".into(), "aws-source".into());
    let configured = ConfiguredAdapters::new(&config, SourceMode::Collect, &CollectorRuntime::default()).unwrap();
    assert_eq!(configured.workspace.tokens_for("fixture").unwrap().auth_kind(), AuthKind::Entra);
    assert_eq!(configured.workspace.tokens_for("rds").unwrap().auth_kind(), AuthKind::AwsIam);
    assert!(!config.state.exists());
}

#[test]
fn invalid_routes_overlapping_state_and_unknown_configuration_fields_are_refused_offline() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let TargetConfig::Adx { query, .. } = &mut config.target else { panic!("ADX fixture") };
    query.endpoint = "https://untrusted.invalid".into();
    assert!(validate(&config).is_err());
    let TargetConfig::Adx { query, .. } = &mut config.target else { panic!("ADX fixture") };
    query.endpoint = "https://never-contact.westeurope.kusto.windows.net".into();
    config.state = config.workspace.join("runtime");
    assert!(validate(&config).is_err());
    config.state = root.path().join("runtime");
    let mut value = serde_json::to_value(&config).unwrap();
    value["target"]["kind"] = serde_json::json!("unregistered-destination");
    assert!(serde_json::from_value::<CollectorConfig>(value).is_err());
    let mut value = serde_json::to_value(&config).unwrap();
    value["silently_ignored_safety_option"] = serde_json::json!(true);
    assert!(serde_json::from_value::<CollectorConfig>(value).is_err());
    assert!(!config.state.exists());
}

#[test]
fn missing_unattended_source_authentication_and_identical_native_and_ready_namespaces_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let storage: AzureBlobConfig = serde_json::from_value(config.recovery.options.clone()).unwrap();
    let TargetConfig::Adx { delivery, .. } = &mut config.target else { panic!("ADX fixture") };
    let AdxDeliveryTransport::EventGrid { ready } = &mut delivery.transport else { panic!("Event Grid fixture") };
    *ready = storage.clone();
    assert!(validate(&config).is_err());
    let TargetConfig::Adx { delivery, .. } = &mut config.target else { panic!("ADX fixture") };
    let AdxDeliveryTransport::EventGrid { ready } = &mut delivery.transport else { panic!("Event Grid fixture") };
    ready.container = "ready".into();
    config.inventory.targets[0].connection = "not-saved".into();
    assert!(validate(&config).is_err());
    assert!(!config.state.exists());
}

#[test]
fn delivery_only_configuration_needs_no_saved_source_or_native_admission_journal() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::remove_dir_all(&config.workspace).unwrap();
    ConfiguredAdapters::new(&config, SourceMode::DeliveryOnly, &CollectorRuntime::default()).unwrap();
    assert!(!config.workspace.exists());
    assert!(!config.state.exists());
    assert!(ConfiguredAdapters::new(&config, SourceMode::Collect, &CollectorRuntime::default()).is_err());
}

#[test]
fn an_unexportable_source_budget_is_rejected_before_any_source_work_or_runtime_state() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.inventory.targets[0].schedules[0].budget.page_rows = 10_000;
    config.inventory.targets[0].schedules[0].budget.max_pages = 100;
    assert!(validate(&config).is_err());
    assert!(!config.state.exists());
    assert!(!config.workspace.join("collection-admission").exists());
}

#[test]
fn role_profiles_are_independent_and_delivery_only_does_not_resolve_source_credentials() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.source_credentials.insert("fixture".into(), "not-configured-source".into());
    assert!(ConfiguredAdapters::new(&config, SourceMode::Collect, &CollectorRuntime::default()).is_err());
    ConfiguredAdapters::new(&config, SourceMode::DeliveryOnly, &CollectorRuntime::default()).unwrap();
    let TargetConfig::Adx { credential, .. } = &mut config.target else { panic!("ADX fixture") };
    *credential = "not-configured-analytics".into();
    assert!(ConfiguredAdapters::new(&config, SourceMode::DeliveryOnly, &CollectorRuntime::default()).is_err());
}
#[cfg(feature = "store-clickhouse")]
#[test]
fn clickhouse_configuration_uses_its_password_profile_without_acquiring_it_offline() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.credentials.insert(
        "warehouse".into(),
        CredentialProfile::PasswordEnvironment {
            username: "fixture".into(),
            variable: "CULPRIT_UNSET_WAREHOUSE_FIXTURE_PASSWORD_6D7B".into(),
        },
    );
    config.target = TargetConfig::ClickHouse {
        query: culprit_store_clickhouse::client::ClickHouseConfig {
            endpoint: "https://never-contact.example".into(),
            database: "fixture".into(),
            insert_quorum: None,
        },
        credential: "warehouse".into(),
    };
    let configured = ConfiguredAdapters::new(&config, SourceMode::Collect, &CollectorRuntime::default()).unwrap();
    assert!(configured.contract.destination().starts_with("clickhouse:"));
    assert_ne!(configured.contract.schema_digest(), culprit_store_adx::catalog::SchemaCatalog::new().unwrap().digest());
    assert!(!config.state.exists());
    let path = root.path().join("collector.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    validate(&CollectorConfig::from_file(&path).unwrap()).unwrap();
    let TargetConfig::ClickHouse { credential, .. } = &mut config.target else { panic!("ClickHouse fixture") };
    *credential = "analytics".into();
    assert!(validate(&config).is_err(), "a token profile cannot silently become a password profile");
}

mod profile_resolution;
