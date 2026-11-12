//! Amazon RDS through the AWS CLI: IAM authentication tokens and the RDS certificate authorities, so no database secret is
//! stored and IAM tokens only travel over verified TLS.

mod access_keys;
use access_keys::CachedKeys;
use culprit_auth::CommandRunner;
use std::sync::Arc;

use culprit_core::{Engine, Error, ErrorCode, Result, aws_iam_host_allowed, t};

/// Amazon's public CA bundle for RDS and Aurora endpoints (truststore.pki.rds.amazonaws.com/global/global-bundle.pem).
const RDS_CA_BUNDLE: &[u8] = include_bytes!("../certs/rds-global-bundle.pem");

/// Connection option value that stands for the bundled RDS certificate authorities.
pub const RDS_CA: &str = "rds";

/// What to do when the AWS CLI isn't signed in.
pub fn sign_in_hint() -> String {
    t!("engine.aws.sign_in_hint")
}

/// How to install the AWS CLI on this platform.
fn install_hint() -> String {
    if cfg!(target_os = "macos") {
        t!("engine.aws.install_hint.macos")
    } else if cfg!(windows) {
        t!("engine.aws.install_hint.windows")
    } else {
        t!("engine.aws.install_hint.linux")
    }
}

pub struct AwsCli {
    runner: Arc<dyn CommandRunner>,
    profile: Option<String>,
    credentials: tokio::sync::Mutex<Option<CachedKeys>>,
}
impl AwsCli {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner, profile: None, credentials: tokio::sync::Mutex::new(None) }
    }
    pub fn with_profile(runner: Arc<dyn CommandRunner>, profile: Option<String>) -> Result<Self> {
        if profile.as_ref().is_some_and(|profile| {
            profile.is_empty()
                || profile.len() > 128
                || !profile.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
        }) {
            return Err(Error::invalid(t!("engine.credentials.invalid_profile")));
        }
        Ok(Self { profile, ..Self::new(runner) })
    }
    /// Run the AWS CLI and return stdout, turning "not installed" and "not signed in" into actionable errors.
    pub async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut arguments = args.to_vec();
        if let Some(profile) = &self.profile {
            arguments.extend(["--profile", profile]);
        }
        let output =
            self.runner.output(&arguments, &[("AWS_PAGER", ""), ("AWS_CLI_AUTO_PROMPT", "off")]).await.map_err(
                |error| {
                    if error.code == ErrorCode::NotFound {
                        Error::new(ErrorCode::AuthFailed, t!("engine.aws.cli_missing")).with_hint(install_hint())
                    } else {
                        error
                    }
                },
            )?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(cli_error(&String::from_utf8_lossy(&output.stderr)))
    }
}

fn cli_error(stderr: &str) -> Error {
    let message = stderr.trim().trim_start_matches("aws: [ERROR]:").trim().to_string();
    if is_sign_in_problem(&message) {
        return Error::new(ErrorCode::AuthFailed, t!("engine.aws.not_signed_in", message = message))
            .with_hint(sign_in_hint());
    }
    if message.contains("AccessDenied")
        || message.contains("UnauthorizedOperation")
        || message.contains("not authorized")
    {
        return Error::new(ErrorCode::PermissionDenied, t!("engine.aws.refused", message = message))
            .with_hint(t!("engine.aws.refused_hint"));
    }
    Error::new(ErrorCode::QueryFailed, t!("engine.aws.cli_failed", message = message))
}

fn is_sign_in_problem(message: &str) -> bool {
    [
        "NoCredentials",
        "Unable to locate credentials",
        "ExpiredToken",
        "Token has expired",
        "token has expired",
        "SSO session",
        "sso session",
        "InvalidClientTokenId",
        "could not be found",
        "aws login",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

pub use culprit_core::cloud::aws::{region_from_host, region_short, valid_region};

pub fn rds_ca_bundle() -> &'static [u8] {
    RDS_CA_BUNDLE
}
impl AwsCli {
    /// A 15-minute RDS IAM authentication token for `user` on `host`, refused unless `host` is an RDS endpoint.
    pub async fn iam_token(&self, engine: Engine, host: &str, port: u16, user: &str, region: &str) -> Result<String> {
        if engine == Engine::Mssql {
            return Err(Error::new(ErrorCode::Unsupported, t!("engine.aws.iam_sql_server"))
                .with_hint(t!("engine.aws.iam_sql_server_hint")));
        }
        if !matches!(engine, Engine::Postgres | Engine::Mysql) {
            return Err(Error::new(ErrorCode::Unsupported, t!("engine.aws.iam_unsupported", engine = engine.label())));
        }
        if !aws_iam_host_allowed(host) {
            return Err(Error::new(ErrorCode::PermissionDenied, t!("engine.aws.iam_host_refused", host = host))
                .with_hint(t!("engine.aws.iam_host_refused_hint")));
        }
        if !valid_region(region) {
            return Err(Error::invalid(t!("engine.aws.bad_region", region = region)));
        }
        let port = port.to_string();
        let stdout = self
            .run(&[
                "rds",
                "generate-db-auth-token",
                "--hostname",
                host,
                "--port",
                &port,
                "--username",
                user,
                "--region",
                region,
                "--output",
                "text",
            ])
            .await?;
        let token = String::from_utf8_lossy(&stdout).trim().to_string();
        if token.is_empty() {
            return Err(Error::new(ErrorCode::AuthFailed, t!("engine.aws.empty_token")).with_hint(sign_in_hint()));
        }
        Ok(token)
    }
}

#[async_trait::async_trait]
impl culprit_auth::DatabaseTokenProvider for AwsCli {
    fn auth_kind(&self) -> culprit_core::AuthKind {
        culprit_core::AuthKind::AwsIam
    }
    async fn database_token(
        &self,
        request: culprit_auth::DatabaseTokenRequest<'_>,
    ) -> Result<culprit_auth::DatabaseCredential> {
        let engine = request.engine;
        let user = request
            .username
            .map(str::to_string)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| Error::invalid(t!("engine.connection.iam_needs_user")))?;
        let port = request.port.unwrap_or(match engine {
            Engine::Postgres => 5432,
            Engine::Yugabyte => {
                return Err(Error::invalid(t!("engine.connection.iam_not_yugabyte")));
            }
            Engine::Mysql => 3306,
            Engine::Mssql => 1433,
            Engine::Sqlite => 0,
        });
        let region =
            request.region.map(str::to_string).or_else(|| region_from_host(request.host)).ok_or_else(|| {
                Error::invalid(t!("engine.connection.unknown_region"))
                    .with_hint(t!("engine.connection.unknown_region_hint"))
            })?;

        let token = self.iam_token(engine, request.host, port, &user, &region).await?;
        Ok(culprit_auth::DatabaseCredential { token: zeroize::Zeroizing::new(token), username: Some(user) })
    }
}

#[async_trait::async_trait]
impl culprit_auth::DatabaseAuthentication for AwsCli {
    fn auth_kind(&self) -> culprit_core::AuthKind {
        culprit_core::AuthKind::AwsIam
    }
    async fn credential(
        &self,
        spec: &culprit_core::ConnectionSpec,
        _: &dyn culprit_auth::SecretStore,
    ) -> Result<culprit_auth::DatabaseCredential> {
        use culprit_auth::{DatabaseTokenProvider, DatabaseTokenRequest};
        self.database_token(DatabaseTokenRequest {
            engine: spec.engine.ok_or_else(|| Error::invalid(t!("engine.connection.unknown_engine")))?,
            host: &spec.host,
            port: spec.port,
            username: spec.user.as_deref(),
            region: spec.option("region"),
        })
        .await
    }
}

#[async_trait::async_trait]
impl culprit_auth::CommandClient for AwsCli {
    async fn run(&self, arguments: &[&str]) -> Result<Vec<u8>> {
        AwsCli::run(self, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoCommands;
    #[async_trait::async_trait]
    impl CommandRunner for NoCommands {
        async fn output(&self, _: &[&str], _: &[(&str, &str)]) -> Result<std::process::Output> {
            panic!("unexpected native command")
        }
    }
    fn cli() -> AwsCli {
        AwsCli::new(Arc::new(NoCommands))
    }

    #[derive(Default)]
    struct SignedRequest(std::sync::Mutex<Vec<Vec<String>>>);
    #[async_trait::async_trait]
    impl CommandRunner for SignedRequest {
        async fn output(&self, arguments: &[&str], _: &[(&str, &str)]) -> Result<std::process::Output> {
            self.0.lock().unwrap().push(arguments.iter().map(|arg| arg.to_string()).collect());
            #[cfg(unix)]
            let status = {
                use std::os::unix::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            #[cfg(windows)]
            let status = {
                use std::os::windows::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            Ok(std::process::Output { status, stdout: b"owned-iam-token\n".to_vec(), stderr: vec![] })
        }
    }
    #[tokio::test]
    async fn shared_database_token_interface_preserves_iam_username_port_and_region() {
        use culprit_auth::{DatabaseTokenProvider, DatabaseTokenRequest};
        let commands = Arc::new(SignedRequest::default());
        let provider = AwsCli::new(commands.clone());
        assert_eq!(provider.auth_kind(), culprit_core::AuthKind::AwsIam);
        let credential = provider
            .database_token(DatabaseTokenRequest {
                engine: Engine::Postgres,
                host: "shop.abc.us-east-1.rds.amazonaws.com",
                port: Some(6432),
                username: Some("app"),
                region: Some("eu-central-1"),
            })
            .await
            .unwrap();
        assert_eq!(credential.token.as_str(), "owned-iam-token");
        assert_eq!(credential.username.as_deref(), Some("app"));
        let requests = commands.0.lock().unwrap();
        for pair in [["--port", "6432"], ["--username", "app"], ["--region", "eu-central-1"]] {
            assert!(requests[0].windows(2).any(|arguments| arguments == pair));
        }
    }

    #[test]
    fn regions_from_rds_hosts() {
        assert_eq!(region_from_host("shop.abc123.us-east-1.rds.amazonaws.com").as_deref(), Some("us-east-1"));
        assert_eq!(
            region_from_host("shop.cluster-ro-x.eu-central-1.rds.amazonaws.com").as_deref(),
            Some("eu-central-1")
        );
        assert_eq!(region_from_host("shop.abc.cn-north-1.rds.amazonaws.com.cn").as_deref(), Some("cn-north-1"));
        assert_eq!(region_from_host("localhost"), None);
        assert!(valid_region("us-gov-west-1"));
        assert!(!valid_region("us-east-1; rm -rf"));
    }

    #[test]
    fn short_region_codes() {
        assert_eq!(region_short("us-east-1"), "use1");
        assert_eq!(region_short("eu-central-1"), "euc1");
        assert_eq!(region_short("ap-southeast-2"), "apse2");
        assert_eq!(region_short("us-gov-west-1"), "usgw1");
    }

    #[test]
    fn sign_in_errors_are_actionable() {
        let e = cli_error(
            "aws: [ERROR]: An error occurred (NoCredentials): Unable to locate credentials. You can configure credentials by running \"aws login\".",
        );
        assert_eq!(e.code, ErrorCode::AuthFailed);
        assert_eq!(e.hint, Some(sign_in_hint()));
        let e = cli_error("An error occurred (AccessDenied) when calling the DescribeDBInstances operation");
        assert_eq!(e.code, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn iam_tokens_are_refused_before_calling_aws() {
        let e = cli()
            .iam_token(Engine::Postgres, "collector.attacker.example", 5432, "app", "us-east-1")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        let e = cli()
            .iam_token(Engine::Mssql, "shop.abc.us-east-1.rds.amazonaws.com", 1433, "app", "us-east-1")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        let e = cli()
            .iam_token(Engine::Postgres, "shop.abc.us-east-1.rds.amazonaws.com", 5432, "app", "x; y")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn bundled_rds_authorities_are_pem() {
        let text = String::from_utf8_lossy(RDS_CA_BUNDLE);
        assert!(text.matches("BEGIN CERTIFICATE").count() > 10);
    }

    // What a person reads, word for word: its messages come from locales/en.json.
    #[test]
    fn aws_cli_failures_say_what_to_do_in_english() {
        let e = cli_error("aws: [ERROR]: Unable to locate credentials");
        assert_eq!(e.code, ErrorCode::AuthFailed);
        assert_eq!(e.message, "The AWS CLI is not signed in: Unable to locate credentials");
        assert_eq!(
            e.hint.as_deref(),
            Some("Sign in with `aws login` (or `aws configure sso` / `aws configure`), then search again")
        );
        let e = cli_error("An error occurred (AccessDenied) when calling DescribeDBInstances");
        assert_eq!(
            e.message,
            "AWS refused the request: An error occurred (AccessDenied) when calling DescribeDBInstances"
        );
        assert_eq!(
            e.hint.as_deref(),
            Some("The signed-in AWS identity needs rds:Describe* (and rds-db:connect for IAM database authentication)")
        );
        let e = cli_error("boom");
        assert_eq!((e.message.as_str(), e.hint), ("AWS CLI call failed: boom", None));
    }

    #[tokio::test]
    async fn iam_tokens_refused_before_the_cli_say_why_in_english() {
        let host = "shop.abc123.us-east-1.rds.amazonaws.com";
        let e = cli().iam_token(Engine::Mssql, host, 1433, "app", "us-east-1").await.unwrap_err();
        assert_eq!(e.message, "Amazon RDS for SQL Server does not support IAM database authentication");
        assert_eq!(e.hint.as_deref(), Some("Use password authentication for this connection"));
        let e = cli().iam_token(Engine::Postgres, "db.example.com", 5432, "app", "us-east-1").await.unwrap_err();
        assert_eq!(
            e.message,
            "Refusing to send an AWS IAM token to 'db.example.com', which is not an Amazon RDS endpoint"
        );
        assert_eq!(
            e.hint.as_deref(),
            Some(
                "IAM database authentication only works with *.rds.amazonaws.com hosts; use password authentication for \
                 other servers"
            )
        );
        let e = cli().iam_token(Engine::Postgres, host, 5432, "app", "nowhere").await.unwrap_err();
        assert_eq!(e.message, "'nowhere' is not an AWS region");
    }
}
