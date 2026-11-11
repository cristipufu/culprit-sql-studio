use super::*;
use crate::{NewConnection, azure_blob::AzureBlobConfig, azure_identity::IdentityConfig};
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
        identity: IdentityConfig::ManagedIdentity { client_id: None },
        recovery: RecoveryConfig::AzureBlob {
            storage: AzureBlobConfig {
                account: "nevercontact".into(),
                container: "recovery".into(),
                prefix: "test/alpha".into(),
            },
        },
        target: TargetConfig::Adx {
            query: crate::adx::client::AdxConfig {
                endpoint: "https://never-contact.westeurope.kusto.windows.net".into(),
                database: "test".into(),
            },
            ready: AzureBlobConfig {
                account: "nevercontact".into(),
                container: "ready".into(),
                prefix: "test/alpha".into(),
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
    assert!(!config.state.exists());
    assert!(!config.workspace.join("source-admission").exists());
    let path = root.path().join("collector.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    validate(&CollectorConfig::from_file(&path).unwrap()).unwrap();
    assert!(!config.state.exists());
}

#[test]
fn invalid_routes_overlapping_state_and_unknown_configuration_fields_are_refused_offline() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let TargetConfig::Adx { query, .. } = &mut config.target;
    query.endpoint = "https://untrusted.invalid".into();
    assert!(validate(&config).is_err());
    let TargetConfig::Adx { query, .. } = &mut config.target;
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
    let RecoveryConfig::AzureBlob { storage } = &config.recovery;
    let TargetConfig::Adx { ready, .. } = &mut config.target;
    *ready = storage.clone();
    assert!(validate(&config).is_err());
    let TargetConfig::Adx { ready, .. } = &mut config.target;
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
    ConfiguredAdapters::new(&config, SourceMode::DeliveryOnly).unwrap();
    assert!(!config.workspace.exists());
    assert!(!config.state.exists());
    assert!(ConfiguredAdapters::new(&config, SourceMode::Collect).is_err());
}

#[test]
fn an_unexportable_source_budget_is_rejected_before_any_source_work_or_runtime_state() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.inventory.targets[0].schedules[0].budget.page_rows = 10_000;
    config.inventory.targets[0].schedules[0].budget.max_pages = 100;
    assert!(validate(&config).is_err());
    assert!(!config.state.exists());
    assert!(!config.workspace.join("source-admission").exists());
}
