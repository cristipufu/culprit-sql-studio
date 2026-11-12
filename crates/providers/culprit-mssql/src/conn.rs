use std::sync::{Arc, Mutex};
use std::time::Duration;

use culprit_core::{
    AuthKind, ConnectionEndpoint, ConnectionSpec, ConnectionTransport, Engine, Error, ErrorCode, Result,
    entra_host_allowed, t,
};
use futures_util::future::BoxFuture;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

#[cfg(test)]
use crate::value::Rec;

pub(crate) type MsClient = Client<Compat<TcpStream>>;
#[cfg(test)]
mod native_tests;

pub(crate) const DEFAULT_PORT: u16 = 1433;
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_IDLE: usize = 4;

/// Bound lock waits and prefer this session as a deadlock victim. Read-uncommitted avoids shared data locks;
/// schema and catalog locks are still possible, and these settings do not limit CPU or logical reads.
const STATS_SESSION_SETUP: &str =
    "SET LOCK_TIMEOUT 2000; SET DEADLOCK_PRIORITY LOW; SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;";
/// Applied to sessions that run user statements: same protections, default isolation.
const USER_SESSION_SETUP: &str = "SET LOCK_TIMEOUT 2000; SET DEADLOCK_PRIORITY LOW;";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionKind {
    Stats,
    User,
}

pub(crate) fn build_config(spec: &ConnectionSpec) -> Result<Config> {
    spec.check_transport()?;
    if spec.host.trim().is_empty() {
        return Err(Error::invalid(t!("provider.mssql.connect.host_required")));
    }
    let mut config = Config::new();
    config.host(spec.host.trim());
    config.port(spec.port.unwrap_or(DEFAULT_PORT));
    if let Some(database) = spec.database.as_deref().filter(|d| !d.is_empty()) {
        config.database(database);
    }
    config.application_name("culprit");
    config.handshake_timeout(Some(HANDSHAKE_TIMEOUT));
    config.command_timeout(Some(COMMAND_TIMEOUT));
    config.authentication(auth_method(spec)?);
    let entra = spec.auth == AuthKind::Entra;
    let trust_any_certificate = spec.flag("trustServerCertificate").unwrap_or(false);
    if entra && trust_any_certificate {
        return Err(Error::invalid(t!("provider.mssql.connect.entra_certificate"))
            .with_hint(t!("provider.mssql.connect.entra_certificate.hint")));
    }
    let level = encryption_level(spec);
    config.encryption(if entra && level == EncryptionLevel::Off { EncryptionLevel::Required } else { level });
    if trust_any_certificate {
        config.trust_cert();
    }
    if let Some(path) = spec.option("trustServerCertificateCA").filter(|p| !p.is_empty()) {
        config.trust_cert_ca(path);
    }
    if let Some(name) = spec.option("hostNameInCertificate") {
        config.hostname_in_certificate(name);
    }
    if spec.option("applicationIntent").is_some_and(|v| v.eq_ignore_ascii_case("readonly")) {
        config.readonly(true);
    }
    Ok(config)
}

fn auth_method(spec: &ConnectionSpec) -> Result<AuthMethod> {
    match spec.auth {
        AuthKind::Password => {
            let user = spec
                .user
                .as_deref()
                .filter(|u| !u.is_empty())
                .ok_or_else(|| Error::invalid(t!("provider.mssql.connect.user_required")))?;
            Ok(AuthMethod::sql_server(user, spec.password.as_deref().unwrap_or("")))
        }
        AuthKind::Entra => {
            if !entra_host_allowed(Engine::Mssql, &spec.host) {
                return Err(Error::new(ErrorCode::PermissionDenied, t!("provider.mssql.connect.entra_host")));
            }
            let token = spec.token.as_deref().ok_or_else(|| {
                Error::new(ErrorCode::AuthFailed, t!("provider.mssql.connect.no_entra_token"))
                    .with_hint(t!("provider.mssql.connect.no_entra_token.hint"))
            })?;
            Ok(AuthMethod::aad_token(token))
        }
        AuthKind::AwsIam => Err(Error::new(ErrorCode::Unsupported, t!("provider.mssql.connect.no_iam"))
            .with_hint(t!("provider.mssql.connect.no_iam.hint"))),
        AuthKind::None => Err(Error::invalid(t!("provider.mssql.connect.no_auth"))),
    }
}

fn encryption_level(spec: &ConnectionSpec) -> EncryptionLevel {
    let security = Engine::Mssql.spec().connection_security().expect("SQL Server has network security settings");
    let value = spec.option("encrypt").unwrap_or(&security.default_mode);
    match value.trim().to_ascii_lowercase().as_str() {
        "strict" => EncryptionLevel::Strict,
        "false" | "no" | "0" | "optional" => EncryptionLevel::Off,
        _ => EncryptionLevel::Required,
    }
}

/// Azure SQL gateways redirect to worker hosts in the same domain; a redirect that leaves it would carry the login (password or
/// Entra token) to someone else.
fn redirect_allowed(original_addr: &str, target_host: &str) -> bool {
    let original_host = original_addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(original_addr);
    if !entra_host_allowed(Engine::Mssql, original_host) {
        return true;
    }
    entra_host_allowed(Engine::Mssql, target_host)
}

async fn endpoint_for(
    config: &Config,
    transport: Option<&dyn ConnectionTransport>,
) -> Result<Option<ConnectionEndpoint>> {
    let Some(transport) = transport else { return Ok(None) };
    let addr = config.get_addr();
    let (host, port) =
        addr.rsplit_once(':').ok_or_else(|| Error::invalid(t!("provider.mssql.connect.invalid_endpoint")))?;
    let port = port.parse().map_err(|_| Error::invalid(t!("provider.mssql.connect.invalid_port")))?;
    transport.endpoint(host, port).await.map(Some)
}

/// Login can execute server logon triggers; session setup also sends SQL. Losing either response is native uncertainty.
struct ConnectionWork<'a> {
    pool: &'a Pool,
    started: bool,
}
impl ConnectionWork<'_> {
    fn begin_login(&mut self) {
        self.started = true;
    }
    fn confirmed(&mut self) {
        self.started = false;
    }
}
impl Drop for ConnectionWork<'_> {
    fn drop(&mut self) {
        if self.started {
            self.pool.mark_uncertain();
        }
    }
}

async fn open_tcp(
    config: Config,
    endpoint: Option<&ConnectionEndpoint>,
    work: &mut ConnectionWork<'_>,
) -> tiberius::Result<MsClient> {
    let addr = config.get_addr();
    let dial = endpoint.map_or_else(|| addr.clone(), |endpoint| endpoint.address.to_string());
    let tcp = tokio::time::timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(&dial)).await.map_err(|_| {
        tiberius::error::Error::Io { kind: std::io::ErrorKind::TimedOut, message: connect_timed_out(&addr) }
    })??;
    tcp.set_nodelay(true)?;
    work.begin_login();
    Client::connect(config, tcp.compat_write()).await
}

/// The TCP connection to `addr` didn't open in time: the message of the I/O error it becomes.
pub(crate) fn connect_timed_out(addr: &str) -> String {
    t!("provider.mssql.connect.timed_out", address = addr)
}

/// A gateway redirected the login outside Azure SQL, where it would carry the password or token to someone else.
pub(crate) fn redirect_refused(host: &str) -> Error {
    Error::new(ErrorCode::ConnectionFailed, t!("provider.mssql.connect.redirect_refused", host = host))
}

/// Connect, following one Azure SQL gateway redirect, and prepare the session.
async fn connect(
    config: &Config,
    transport: Option<&dyn ConnectionTransport>,
    kind: SessionKind,
    work: &mut ConnectionWork<'_>,
) -> Result<MsClient> {
    let mut endpoint = endpoint_for(config, transport).await?;
    let attempt = match open_tcp(config.clone(), endpoint.as_ref(), work).await {
        Err(tiberius::error::Error::Routing { host, port }) => {
            // A typed routing response completed at the gateway before a database session was admitted.
            work.confirmed();
            if !redirect_allowed(&config.get_addr(), &host) {
                return Err(redirect_refused(&host));
            }
            let mut routed = config.clone();
            routed.host(&host);
            routed.port(port);
            endpoint = endpoint_for(&routed, transport).await?;
            open_tcp(routed, endpoint.as_ref(), work).await
        }
        other => other,
    };
    let mut client = attempt.map_err(|error| culprit_core::transport_error(endpoint.as_ref(), map_error(error)))?;
    let setup = match kind {
        SessionKind::Stats => STATS_SESSION_SETUP,
        SessionKind::User => USER_SESSION_SETUP,
    };
    execute(&mut client, setup).await.map_err(map_error)?;
    work.confirmed();
    Ok(client)
}

pub(crate) async fn execute(client: &mut MsClient, sql: &str) -> tiberius::Result<()> {
    client.simple_query(crate::statement_origin::requested_batch(sql)).await?.into_results().await?;
    Ok(())
}

/// Marks Culprit's own statements so workload collection can leave them out of the application's history.
#[cfg(test)]
pub(crate) async fn query(client: &mut MsClient, sql: &str) -> tiberius::Result<Vec<Rec>> {
    let rows = client.simple_query(crate::statement_origin::monitoring_batch(sql)).await?.into_first_result().await?;
    Ok(rows.iter().map(Rec::from_row).collect())
}

#[cfg(test)]
pub(crate) async fn query_one(client: &mut MsClient, sql: &str) -> tiberius::Result<Rec> {
    Ok(query(client, sql).await?.into_iter().next().unwrap_or_default())
}

/// Reuses statistics sessions so repeated calls (Live refreshes every couple of seconds) skip the TLS and login round-trips.
pub(crate) struct Pool {
    config: Config,
    transport: Option<Arc<dyn ConnectionTransport>>,
    idle: Mutex<Vec<MsClient>>,
    uncertain: culprit_read::ReadFence,
}

impl Pool {
    pub(crate) fn new(config: Config, transport: Option<Arc<dyn ConnectionTransport>>) -> Self {
        Pool { config, transport, idle: Mutex::new(Vec::new()), uncertain: culprit_read::ReadFence::default() }
    }

    pub(crate) async fn connect(&self, kind: SessionKind) -> Result<MsClient> {
        self.ensure_available()?;
        let mut work = ConnectionWork { pool: self, started: false };
        connect(&self.config, self.transport.as_deref(), kind, &mut work).await
    }

    pub(crate) fn mark_uncertain(&self) {
        self.uncertain.mark_uncertain();
    }
    pub(crate) fn ensure_available(&self) -> Result<()> {
        self.uncertain.ensure_available()
    }
    pub(crate) fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        if self.ensure_available().is_ok() {
            culprit_core::telemetry::SourceShutdown::Quiescent
        } else {
            culprit_core::telemetry::SourceShutdown::Unconfirmed
        }
    }
    pub(crate) fn transport_owner(&self) -> Option<Arc<dyn ConnectionTransport>> {
        self.transport.clone()
    }

    fn take_idle(&self) -> Option<MsClient> {
        self.idle.lock().ok()?.pop()
    }

    pub(crate) async fn get(&self) -> Result<MsClient> {
        self.ensure_available()?;
        if let Some(client) = self.take_idle() {
            return Ok(client);
        }
        self.connect(SessionKind::Stats).await
    }

    pub(crate) fn put(&self, client: MsClient) {
        if self.ensure_available().is_err() {
            return;
        }
        let Ok(mut idle) = self.idle.lock() else { return };
        if idle.len() < MAX_IDLE {
            idle.push(client);
        }
    }

    /// All statistical callbacks use owned native cancellation and retain shutdown uncertainty in the pool.
    pub(crate) async fn observe<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        F: for<'a> FnOnce(&'a mut crate::read::ReadSession) -> BoxFuture<'a, Result<T>>,
    {
        let mut reader = crate::read::ReadSession::application(self.clone());
        let result = f(&mut reader).await;
        reader.finish(result).await
    }

    /// Run exactly once. A transport error cannot prove that submitted work stopped, so it must not trigger replay.
    pub(crate) async fn with<T, F>(&self, f: F) -> Result<T>
    where
        F: for<'c> FnOnce(&'c mut MsClient) -> BoxFuture<'c, tiberius::Result<T>>,
    {
        let mut client = self.get().await?;
        match f(&mut client).await {
            Ok(value) => {
                self.put(client);
                Ok(value)
            }
            Err(err) => Err(map_error(err)),
        }
    }
}

/// What a login without the monitoring permission is told to ask for.
pub(crate) fn permission_hint() -> String {
    t!("provider.mssql.error.permission.hint")
}

/// An error the server returned (`code`, its `message`), with what to do about it where Culprit knows.
pub(crate) fn server_error(code: u32, message: String) -> Error {
    match code {
        18456 | 18452 | 18470 | 18486 | 18487 | 18488 => {
            let hint = if message.contains("token-identified principal") {
                t!("provider.mssql.error.entra_user.hint")
            } else {
                t!("provider.mssql.error.login.hint")
            };
            Error::new(ErrorCode::AuthFailed, message).with_hint(hint)
        }
        4060 => Error::new(ErrorCode::NotFound, message).with_hint(t!("provider.mssql.error.database.hint")),
        40615 | 40914 => {
            Error::new(ErrorCode::ConnectionFailed, message).with_hint(t!("provider.mssql.error.firewall.hint"))
        }
        40613 | 40501 | 40197 | 49918 | 49919 | 49920 => {
            Error::new(ErrorCode::ConnectionFailed, message).with_hint(t!("provider.mssql.error.unavailable.hint"))
        }
        1222 => Error::new(ErrorCode::Timeout, message).with_hint(t!("provider.mssql.error.lock_timeout.hint")),
        229 | 230 | 262 | 297 | 300 | 15151 | 15247 => {
            Error::new(ErrorCode::PermissionDenied, message).with_hint(permission_hint())
        }
        _ => Error::new(ErrorCode::QueryFailed, message),
    }
}

pub(crate) fn map_error(err: tiberius::error::Error) -> Error {
    use tiberius::error::Error as T;
    match &err {
        T::Server(token) => server_error(token.code(), token.message().to_string()),
        T::Io { kind, message } if *kind == std::io::ErrorKind::TimedOut => {
            Error::new(ErrorCode::Timeout, t!("provider.mssql.error.timeout", error = message))
                .with_hint(t!("provider.mssql.error.timeout.hint"))
        }
        T::Io { message, .. } => {
            Error::new(ErrorCode::ConnectionFailed, t!("provider.mssql.error.unreachable", error = message))
                .with_hint(t!("provider.mssql.error.unreachable.hint"))
        }
        T::Tls(message) => Error::new(ErrorCode::ConnectionFailed, t!("provider.mssql.error.tls", error = message))
            .with_hint(t!("provider.mssql.error.tls.hint"))
            .with_detail(culprit_core::ErrorDetail::ConnectionSecurity),
        T::Routing { host, port } => Error::new(
            ErrorCode::ConnectionFailed,
            t!("provider.mssql.error.redirected_twice", host = host, port = port.to_string()),
        ),
        _ => Error::new(ErrorCode::Internal, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement_origin::{MONITORING_QUERY_MARKER, OWN_QUERY_MARKER};

    struct Route(Mutex<Vec<(String, u16)>>);
    #[async_trait::async_trait]
    impl ConnectionTransport for Route {
        async fn endpoint(&self, host: &str, port: u16) -> Result<ConnectionEndpoint> {
            self.0.lock().unwrap().push((host.into(), port));
            Ok(ConnectionEndpoint::new("127.0.0.1:11433".parse().unwrap()))
        }
    }

    #[tokio::test]
    async fn gateway_redirects_use_the_route_without_changing_tls_identity_to_loopback() {
        let spec = ConnectionSpec { host: "shop.database.windows.net".into(), ..spec() };
        let config = build_config(&spec).unwrap();
        let route = Route(Mutex::new(Vec::new()));
        assert_eq!(endpoint_for(&config, Some(&route)).await.unwrap().unwrap().address.port(), 11433);
        assert_eq!(config.get_addr(), "shop.database.windows.net:1433");
        let mut routed = config.clone();
        routed.host("worker.database.windows.net");
        routed.port(11001);
        assert!(redirect_allowed(&config.get_addr(), "worker.database.windows.net"));
        assert!(!redirect_allowed(&config.get_addr(), "attacker.example"));
        assert_eq!(endpoint_for(&routed, Some(&route)).await.unwrap().unwrap().address.port(), 11433);
        assert_eq!(routed.get_addr(), "worker.database.windows.net:11001");
        assert_eq!(
            *route.0.lock().unwrap(),
            vec![("shop.database.windows.net".into(), 1433), ("worker.database.windows.net".into(), 11001)]
        );
    }

    fn spec() -> ConnectionSpec {
        ConnectionSpec {
            host: "db.example.com".into(),
            user: Some("reader".into()),
            password: Some("secret".into()),
            ..Default::default()
        }
    }

    #[test]
    fn monitoring_marks_identify_every_recorded_statement_without_changing_literal_text() {
        for (sql, count) in [
            ("DECLARE @x int=1;SELECT a FROM t", 1),
            ("WITH s AS(SELECT 1 AS a)SELECT a FROM s", 2),
            ("SELECT 1 FROM t WHERE name LIKE N'%select%'", 1),
            ("SET LOCK_TIMEOUT 2000", 1),
            ("DBCC SHOW_STATISTICS(N't',N'i') WITH HISTOGRAM", 1),
            ("SELECTED", 1),
        ] {
            let marked = crate::statement_origin::monitoring_batch(sql);
            let comments = culprit_core::sql::block_comments(&marked, Engine::Mssql).unwrap().unwrap();
            assert_eq!(comments.iter().filter(|comment| **comment == OWN_QUERY_MARKER).count(), count);
            assert_eq!(comments.iter().filter(|comment| **comment == MONITORING_QUERY_MARKER).count(), count);
            let template = format!("/* culprit:template:{} */", crate::statement_origin::template_id(sql));
            assert_eq!(comments.iter().filter(|comment| **comment == template).count(), count);
        }
        let sql = "SELECT 1 FROM t WHERE name LIKE N'%select%'";
        assert!(crate::statement_origin::monitoring_batch(sql).contains("N'%select%'"));
    }

    #[test]
    fn encryption_defaults_to_required() {
        assert!(matches!(encryption_level(&spec()), EncryptionLevel::Required));
        let mut s = spec();
        s.options.insert("Encrypt".into(), "false".into());
        assert!(matches!(encryption_level(&s), EncryptionLevel::Off));
        s.options.insert("Encrypt".into(), "strict".into());
        assert!(matches!(encryption_level(&s), EncryptionLevel::Strict));
    }

    #[test]
    fn password_auth_needs_a_user() {
        let mut s = spec();
        s.user = None;
        assert_eq!(build_config(&s).unwrap_err().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn gateway_redirects_stay_inside_azure_sql() {
        assert!(redirect_allowed("shop.database.windows.net:1433", "tr5.westeurope1-a.worker.database.windows.net"));
        assert!(!redirect_allowed("shop.database.windows.net:1433", "collector.attacker.example"));
        assert!(redirect_allowed("sql01.corp.local:1433", "sql02.corp.local"));
    }

    #[test]
    fn entra_refuses_unverified_certificates() {
        let mut s = spec();
        s.auth = AuthKind::Entra;
        s.host = "shop.database.windows.net".into();
        s.token = Some("t".into());
        s.options.insert("trustServerCertificate".into(), "true".into());
        assert!(build_config(&s).is_err());
    }

    #[test]
    fn entra_tokens_are_refused_for_non_azure_hosts() {
        let mut s = spec();
        s.auth = AuthKind::Entra;
        s.host = "collector.attacker.example".into();
        s.token = Some("t".into());
        assert!(build_config(&s).is_err());
    }

    #[test]
    fn entra_needs_a_token() {
        let mut s = spec();
        s.auth = AuthKind::Entra;
        s.host = "shop.database.windows.net".into();
        assert_eq!(build_config(&s).unwrap_err().code, ErrorCode::AuthFailed);
        s.token = Some("t".into());
        assert!(build_config(&s).is_ok());
    }
}
