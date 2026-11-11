use super::*;
use endpoint::{IdentityReply, IdentityRequest};
use std::collections::VecDeque;

struct Script {
    replies: Mutex<VecDeque<Result<IdentityReply>>>,
    requests: Mutex<Vec<(String, Option<String>)>>,
}
impl Script {
    fn new(replies: Vec<Result<IdentityReply>>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into()), requests: Mutex::new(Vec::new()) })
    }
}
#[async_trait::async_trait]
impl IdentityEndpoint for Script {
    async fn request(&self, request: IdentityRequest) -> Result<IdentityReply> {
        let assertion = match request.config {
            IdentityConfig::WorkloadIdentity { token_file, .. } => {
                Some(endpoint::read_assertion(&token_file)?.to_string())
            }
            IdentityConfig::ManagedIdentity { .. } => None,
        };
        self.requests.lock().unwrap().push((request.audience.resource().into(), assertion));
        self.replies.lock().unwrap().pop_front().expect("unexpected identity request")
    }
}
fn reply(token: &str, lifetime: u64) -> Result<IdentityReply> {
    Ok(IdentityReply {
        status: 200,
        json: serde_json::json!({"access_token":token,"token_type":"Bearer","expires_in":lifetime}),
    })
}
fn workload(path: &std::path::Path) -> IdentityConfig {
    IdentityConfig::WorkloadIdentity {
        tenant_id: "00000000-0000-0000-0000-000000000001".into(),
        client_id: "00000000-0000-0000-0000-000000000002".into(),
        token_file: path.into(),
    }
}

#[tokio::test(start_paused = true)]
async fn token_refresh_reopens_rotated_assertions_and_keeps_audience_caches_separate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token");
    std::fs::write(&path, "projected-assertion-one").unwrap();
    let endpoint =
        Script::new(vec![reply("sql-token-one", 100), reply("storage-token", 100), reply("sql-token-two", 100)]);
    let credential = AzureCredential::with_endpoint(workload(&path), endpoint.clone()).unwrap();
    let sql = Audience::database(Engine::Mssql).unwrap();
    let (a, b) = tokio::join!(credential.token(&sql), credential.token(&sql));
    assert_eq!(a.unwrap(), "sql-token-one");
    assert_eq!(b.unwrap(), "sql-token-one");
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(credential.token(&Audience::storage()).await.unwrap(), "storage-token");
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, "projected-assertion-two").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    tokio::time::advance(Duration::from_secs(81)).await;
    assert_eq!(credential.token(&sql).await.unwrap(), "sql-token-two");
    let requests = endpoint.requests.lock().unwrap();
    assert_eq!(requests[0], (sql.resource().into(), Some("projected-assertion-one".into())));
    assert_eq!(requests[1].0, Audience::storage().resource());
    assert_eq!(requests[2], (sql.resource().into(), Some("projected-assertion-two".into())));
}

#[tokio::test(start_paused = true)]
async fn identity_errors_are_bounded_sanitized_and_retried_after_backoff_without_other_credentials() {
    let endpoint = Script::new(vec![
        Ok(IdentityReply { status: 401, json: serde_json::json!({"error_description":"private-assertion-canary"}) }),
        reply("refreshed-token", 100),
    ]);
    let credential =
        AzureCredential::with_endpoint(IdentityConfig::ManagedIdentity { client_id: None }, endpoint.clone()).unwrap();
    for _ in 0..4 {
        let error = credential.token(&Audience::storage()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthFailed);
        assert!(!error.message.contains("private-assertion-canary"));
    }
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    tokio::time::advance(FAILURE_BACKOFF).await;
    assert_eq!(credential.token(&Audience::storage()).await.unwrap(), "refreshed-token");
    assert_eq!(endpoint.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn hostile_database_hosts_are_rejected_before_any_identity_request() {
    let endpoint = Script::new(vec![]);
    let credential =
        AzureCredential::with_endpoint(IdentityConfig::ManagedIdentity { client_id: None }, endpoint.clone()).unwrap();
    let error = credential.database_token(Engine::Mssql, "database.windows.net.attacker.example").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(endpoint.requests.lock().unwrap().is_empty());
    for endpoint in [
        "https://x.kusto.windows.net@attacker.example",
        "http://x.kusto.windows.net",
        "https://x.kusto.windows.net/path",
        "https://x.kusto.windows.net?token=secret",
    ] {
        assert!(Audience::kusto(endpoint, "https://kusto.kusto.windows.net").is_err());
    }
    assert_eq!(
        Audience::kusto(
            "https://cluster.westeurope.kusto.windows.net/",
            "https://cluster.westeurope.kusto.windows.net"
        )
        .unwrap()
        .resource(),
        "https://cluster.westeurope.kusto.windows.net"
    );
    assert_eq!(
        Audience::kusto("https://cluster.westeurope.kusto.windows.net", "https://kusto.kusto.windows.net")
            .unwrap()
            .resource(),
        "https://kusto.kusto.windows.net"
    );
    for advertised in [
        "https://attacker.example",
        "http://kusto.kusto.windows.net",
        "https://different.kusto.windows.net",
        "https://kusto.kusto.windows.net/.default",
    ] {
        assert!(Audience::kusto("https://cluster.westeurope.kusto.windows.net", advertised).is_err());
    }
}

#[test]
fn invalid_identity_configuration_and_incomplete_token_replies_are_not_accepted() {
    assert!(AzureCredential::new(workload(std::path::Path::new("relative-token"))).is_err());
    assert!(AzureCredential::new(IdentityConfig::ManagedIdentity { client_id: Some("not-a-guid".into()) }).is_err());
    for json in [
        serde_json::json!({"access_token":"token","token_type":"Bearer","expires_in":100,"expires_on":"1"}),
        serde_json::json!({"access_token":"token","expires_in":100}),
        serde_json::json!({"access_token":"token","token_type":"Bearer","expires_in":0}),
        serde_json::json!({"access_token":"token\r\nsecret","token_type":"Bearer","expires_in":100}),
        serde_json::json!({"access_token":"token","token_type":"Bearer","expires_in":100,"resource":"https://database.windows.net/"}),
    ] {
        assert!(parse_token(IdentityReply { status: 200, json }, &Audience::storage(), Instant::now()).is_err());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token");
    for content in [vec![b'x'; 32769], vec![0xff], b"\n\t ".to_vec()] {
        std::fs::write(&path, content).unwrap();
        assert!(endpoint::read_assertion(&path).is_err());
    }
}

#[test]
fn identity_configuration_files_are_bounded_and_do_not_expose_bad_contents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.json");
    std::fs::write(&path, r#"{"kind":"managed-identity","client_id":null}"#).unwrap();
    assert!(AzureCredential::from_file(&path).is_ok());
    for content in
        [b"{\"private-config-canary\":true}".to_vec(), vec![b'x'; MAX_CONFIGURATION_BYTES as usize + 1], vec![0xff]]
    {
        std::fs::write(&path, content).unwrap();
        let error = AzureCredential::from_file(&path).err().unwrap();
        assert!(!error.message.contains("private-config-canary"));
    }
}

#[tokio::test]
async fn workspace_uses_the_injected_identity_for_its_existing_provider_path() {
    let endpoint = Script::new(vec![reply("fixture-sql-token", 100)]);
    let credential = Arc::new(
        AzureCredential::with_endpoint(IdentityConfig::ManagedIdentity { client_id: None }, endpoint.clone()).unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let resolved = Arc::new(Mutex::new(None));
    let captured = resolved.clone();
    let workspace = crate::Workspace::open_in(directory.path()).with_azure_credential(credential).with_provider(
        Engine::Mssql,
        Arc::new(move |spec| {
            *captured.lock().unwrap() = spec.token.clone();
            Err(Error::internal("fixture provider boundary"))
        }),
    );
    workspace
        .add_connection(crate::NewConnection {
            name: "headless".into(),
            input:
                "Server=tcp:fixture.database.windows.net,1433;Database=fixture;Authentication=Active Directory Default"
                    .into(),
            environment: culprit_core::Environment::Staging,
            ..Default::default()
        })
        .unwrap();
    let _ = workspace.connection_service().provider("headless").await.err().unwrap();
    assert_eq!(resolved.lock().unwrap().as_deref(), Some("fixture-sql-token"));
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}
