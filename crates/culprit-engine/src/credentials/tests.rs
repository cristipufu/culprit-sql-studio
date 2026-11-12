use crate::{NewConnection, Workspace};
use culprit_auth::{DatabaseCredential, DatabaseTokenProvider, DatabaseTokenRequest};
use culprit_core::{AuthKind, Engine, Error, Result};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
struct ObservedRequest {
    engine: Engine,
    host: String,
    port: Option<u16>,
    username: Option<String>,
    region: Option<String>,
}
struct Tokens {
    kind: AuthKind,
    requests: Mutex<Vec<ObservedRequest>>,
}
#[async_trait::async_trait]
impl DatabaseTokenProvider for Tokens {
    fn auth_kind(&self) -> AuthKind {
        self.kind
    }
    async fn database_token(&self, request: DatabaseTokenRequest<'_>) -> Result<DatabaseCredential> {
        self.requests.lock().unwrap().push(ObservedRequest {
            engine: request.engine,
            host: request.host.into(),
            port: request.port,
            username: request.username.map(str::to_string),
            region: request.region.map(str::to_string),
        });
        Ok(DatabaseCredential {
            token: Zeroizing::new("owned-token-canary".into()),
            username: Some("token-principal".into()),
        })
    }
}
#[tokio::test]
async fn workspace_uses_the_same_credential_interface_for_entra_and_iam_without_serializing_tokens() {
    for (kind, host, user, principal) in [
        (AuthKind::Entra, "fixture.postgres.database.azure.com", None, "token-principal"),
        (AuthKind::AwsIam, "fixture.abc.eu-central-1.rds.amazonaws.com", Some("explicit-user"), "explicit-user"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let tokens = Arc::new(Tokens { kind, requests: Mutex::new(vec![]) });
        let captured = Arc::new(Mutex::new(None));
        let observed = captured.clone();
        let workspace = Workspace::open_in(directory.path()).with_database_tokens(tokens.clone()).with_provider(
            Engine::Postgres,
            Arc::new(move |spec| {
                assert!(!serde_json::to_string(spec).unwrap().contains("owned-token-canary"));
                *observed.lock().unwrap() = Some((spec.token.clone(), spec.user.clone()));
                Err(Error::internal("owned provider boundary"))
            }),
        );
        workspace
            .add_connection(NewConnection {
                name: "fixture".into(),
                input: format!("host={host} port=6432 dbname=fixture"),
                engine: Some(Engine::Postgres),
                user: user.map(str::to_string),
                auth: Some(kind),
                ..Default::default()
            })
            .unwrap();
        let mut stored = workspace.store.get("fixture").unwrap();
        stored.options.insert("region".into(), "eu-central-1".into());
        workspace.store.put("fixture", stored).unwrap();
        workspace.connection_service().provider("fixture").await.err().unwrap();
        assert_eq!(*captured.lock().unwrap(), Some((Some("owned-token-canary".into()), Some(principal.into()))));
        assert_eq!(
            *tokens.requests.lock().unwrap(),
            vec![ObservedRequest {
                engine: Engine::Postgres,
                host: host.into(),
                port: Some(6432),
                username: user.map(str::to_string),
                region: Some("eu-central-1".into())
            }]
        );
    }
}
#[tokio::test]
async fn an_injected_provider_for_another_authentication_mode_is_rejected_before_acquisition() {
    let directory = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens { kind: AuthKind::AwsIam, requests: Mutex::new(vec![]) });
    let workspace = Workspace::open_in(directory.path()).with_database_tokens(tokens.clone());
    workspace
        .add_connection(NewConnection {
            name: "fixture".into(),
            input: "host=fixture.postgres.database.azure.com dbname=fixture".into(),
            engine: Some(Engine::Postgres),
            auth: Some(AuthKind::Entra),
            ..Default::default()
        })
        .unwrap();
    let error = workspace.connection_service().provider("fixture").await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
    assert!(tokens.requests.lock().unwrap().is_empty());
}

#[derive(Default)]
struct AuthenticationLifecycle(Mutex<Vec<&'static str>>);
#[async_trait::async_trait]
impl culprit_auth::DatabaseAuthentication for AuthenticationLifecycle {
    fn auth_kind(&self) -> AuthKind {
        AuthKind::Entra
    }
    async fn credential(
        &self,
        _: &culprit_core::ConnectionSpec,
        _: &dyn culprit_auth::SecretStore,
    ) -> Result<DatabaseCredential> {
        self.0.lock().unwrap().push("credential");
        Ok(DatabaseCredential { token: Zeroizing::new("plugin-token".into()), username: Some("plugin-user".into()) })
    }
    fn set_sign_in_handler(&self, handler: Option<culprit_auth::SignInHandler>) {
        assert!(handler.is_some());
        self.0.lock().unwrap().push("handler");
    }
    fn cancel_sign_in(&self) {
        self.0.lock().unwrap().push("cancel");
    }
    fn sign_out(
        &self,
        connections: &[culprit_core::ConnectionSpec],
        _: &dyn culprit_auth::SecretStore,
    ) -> Result<usize> {
        assert_eq!(connections.len(), 1);
        self.0.lock().unwrap().push("sign-out");
        Ok(3)
    }
}
#[tokio::test]
async fn authentication_plugins_own_credential_acquisition_and_the_entire_interactive_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let authentication = Arc::new(AuthenticationLifecycle::default());
    let workspace = Workspace::open_in(directory.path())
        .with_authentication_provider(authentication.clone())
        .with_sign_in_handler(Arc::new(|_| {}))
        .with_provider(
            Engine::Postgres,
            Arc::new(|spec| {
                assert_eq!(spec.token.as_deref(), Some("plugin-token"));
                assert_eq!(spec.user.as_deref(), Some("plugin-user"));
                Err(Error::internal("owned provider boundary"))
            }),
        );
    workspace
        .add_connection(NewConnection {
            name: "fixture".into(),
            input: "host=fixture.postgres.database.azure.com dbname=fixture".into(),
            engine: Some(Engine::Postgres),
            auth: Some(AuthKind::Entra),
            ..Default::default()
        })
        .unwrap();
    workspace.connection_service().provider("fixture").await.err().unwrap();
    workspace.cancel_sign_in();
    assert_eq!(workspace.sign_out().unwrap(), 3);
    assert_eq!(*authentication.0.lock().unwrap(), ["handler", "credential", "cancel", "sign-out"]);
}
