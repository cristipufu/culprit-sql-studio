//! The smoke supervisor owns all channel/account setup and container cleanup. These tests only read it.
use super::*;
use crate::{connect::Scope, native::metadata::ConnectionContext, read::ReadSession};
use culprit_core::{ConnectionSpec, Engine};

pub(super) enum Login {
    Monitor,
    Denied,
}

pub(super) struct Fixture {
    specification: ConnectionSpec,
    pub channels: Vec<String>,
    pub native: ConnectionContext,
}

impl Fixture {
    pub async fn open() -> Self {
        let owner = environment("CULPRIT_SMOKE_OWNER");
        assert!(valid_owner(&owner), "requires the owned smoke fixture nonce");
        let channels =
            environment("CULPRIT_MYSQL_REPLICATION_CHANNELS").split(',').map(String::from).collect::<Vec<_>>();
        assert!(valid_channels(&channels, &owner), "requires two distinct nonce-owned replication channels");
        let port = environment("CULPRIT_MYSQL_TEST_PORT").parse::<u16>().expect("owned fixture port");
        assert!(port > 0, "the supervisor must publish a loopback port");
        let specification = ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(port),
            database: Some("culprit_smoke".into()),
            user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or_else(|_| "root".into())),
            password: Some(environment("CULPRIT_MYSQL_TEST_PASSWORD")),
            ..Default::default()
        };
        let provider = required(crate::MysqlProvider::new(&specification), "owned fixture construction");
        let mut reader = ReadSession::new(provider.connector.clone(), budget(), Scope::Server);
        let native = crate::native::metadata::connection_context(&mut reader, &provider.connector).await;
        let shutdown = reader.close().await;
        assert_eq!(shutdown, SourceShutdown::Quiescent, "fixture guard read must settle");
        let native = required(native, "owned fixture guard metadata");
        assert!(
            native.server_name.starts_with("culprit-smoke-") && native.server_name.ends_with(&format!("-{owner}")),
            "native hostname must authenticate the supervisor's exact disposable fixture"
        );
        assert_eq!(native.database_name, "culprit_smoke");
        if let Ok(expected) = std::env::var("CULPRIT_MYSQL_REPLICATION_PERFORMANCE_SCHEMA") {
            assert!(matches!(expected.as_str(), "on" | "off"), "explicit instrumentation fixture state");
            assert_eq!(native.performance_schema, expected == "on");
        }
        Self { specification, channels, native }
    }

    pub fn provider(&self, login: Login) -> crate::MysqlProvider {
        let mut specification = self.specification.clone();
        let (user, password) = match login {
            Login::Monitor => ("CULPRIT_MYSQL_REPLICATION_MONITOR_USER", "CULPRIT_MYSQL_REPLICATION_MONITOR_PASSWORD"),
            Login::Denied => ("CULPRIT_SMOKE_LIMITED_USER", "CULPRIT_SMOKE_LIMITED_PASSWORD"),
        };
        specification.user = Some(environment(user));
        specification.password = Some(environment(password));
        required(crate::MysqlProvider::new(&specification), "fixture provider construction")
    }
}

pub(super) fn budget() -> CollectionBudget {
    CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 8,
        max_queries: 32,
        total_timeout_ms: 30_000,
        ..Default::default()
    }
}

pub(in crate::telemetry::native_tests) fn required<T>(result: Result<T>, phase: &str) -> T {
    match result {
        Ok(value) => value,
        // Native error text can contain a user, endpoint or SQL. Conformance failures expose only the code.
        Err(error) => panic!("{phase} failed ({:?}); native error text withheld", error.code),
    }
}

fn environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing owned fixture environment {name}"))
}

pub(in crate::telemetry::native_tests) fn valid_owner(owner: &str) -> bool {
    owner.len() == 24 && owner.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_channels(channels: &[String], owner: &str) -> bool {
    channels.len() == 2
        && channels[0] != channels[1]
        && channels.iter().all(|channel| {
            channel.len() <= 64
                && channel.starts_with("culprit_")
                && channel.contains(owner)
                && channel.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

#[test]
fn fixture_admission_requires_an_exact_nonce_and_two_safe_distinct_owned_channel_names() {
    let owner = "0123456789abcdef01234567";
    assert!(valid_owner(owner));
    for invalid in ["", "0123456789abcdef0123456", "0123456789ABCDEF01234567", "0123456789abcdef0123456g"] {
        assert!(!valid_owner(invalid));
    }
    let channels = vec![format!("culprit_{owner}_stopped"), format!("culprit_{owner}_io_error")];
    assert!(valid_channels(&channels, owner));
    assert!(!valid_channels(&[channels[0].clone(), channels[0].clone()], owner));
    assert!(!valid_channels(&[channels[0].clone(), "unrelated_channel".into()], owner));
    assert!(!valid_channels(&[channels[0].clone(), format!("culprit_{owner}_';DROP")], owner));
}
