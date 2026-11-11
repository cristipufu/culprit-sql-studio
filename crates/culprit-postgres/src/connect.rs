use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use culprit_core::{
    AuthKind, ConnectionEndpoint, ConnectionSpec, ConnectionTransport, Engine, Error, ErrorCode, Result,
    aws_iam_host_allowed, entra_host_allowed, t, transport_error,
};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_postgres::config::SslMode;
use tokio_postgres::{AsyncMessage, Client};
use tokio_postgres_rustls::MakeRustlsConnect;

pub const DEFAULT_PORT: u16 = 5432;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeouts applied to a session. Collectors read bigger catalogs than interactive calls.
#[derive(Clone, Copy)]
pub struct SessionLimits {
    pub statement_timeout: Duration,
    /// Culprit's own statements only (a scan): names resolve in the catalog alone, whatever search_path the database
    /// or role sets, so no schema someone else controls can stand in for a catalog view, function or operator. The
    /// sessions that run what a person writes keep the database's search_path.
    pub catalog_path: bool,
}

pub const INTERACTIVE: SessionLimits =
    SessionLimits { statement_timeout: culprit_core::limits::OBSERVE_TIMEOUT, catalog_path: false };
pub const COLLECTOR: SessionLimits = SessionLimits { statement_timeout: Duration::from_secs(20), catalog_path: true };
pub const EXECUTION: SessionLimits =
    SessionLimits { statement_timeout: culprit_core::limits::QUERY_TIMEOUT, catalog_path: false };

#[derive(Clone)]
pub struct Connector {
    config: tokio_postgres::Config,
    tls: MakeRustlsConnect,
    transport: Option<Arc<dyn ConnectionTransport>>,
    pub(crate) fence: culprit_read::ReadFence,
    pub host: String,
    pub port: u16,
}

/// One connection's server notices, without retaining server-controlled message text. Distributed statistics can
/// return rows after skipping a node: callers must distinguish that partial read from an empty successful read.
pub struct ObservedSession {
    pub client: Client,
    pub(crate) notices: Arc<AtomicU64>,
}

impl ObservedSession {
    pub fn notices(&self) -> u64 {
        self.notices.load(Ordering::SeqCst)
    }
}

impl Connector {
    pub fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        if self.fence.is_uncertain() {
            culprit_core::telemetry::SourceShutdown::Unconfirmed
        } else {
            culprit_core::telemetry::SourceShutdown::Quiescent
        }
    }

    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        spec.check_transport()?;
        if spec.host.trim().is_empty() {
            return Err(Error::invalid(t!("provider.postgres.connect.no_host")));
        }
        let port = spec.port.unwrap_or(DEFAULT_PORT);
        let mut config = tokio_postgres::Config::new();
        config.host(spec.host.trim()).application_name("culprit").connect_timeout(CONNECT_TIMEOUT).keepalives(true);
        if let Some(database) = spec.database.as_deref().filter(|d| !d.is_empty()) {
            config.dbname(database);
        }
        if let Some(user) = spec.user.as_deref().filter(|u| !u.is_empty()) {
            config.user(user);
        }
        match spec.auth {
            AuthKind::Password => {
                if let Some(password) = &spec.password {
                    config.password(password.as_bytes());
                }
            }
            AuthKind::Entra => {
                if !entra_host_allowed(Engine::Postgres, &spec.host) {
                    return Err(Error::new(ErrorCode::PermissionDenied, t!("provider.postgres.connect.entra_host")));
                }
                let token = spec.token.as_ref().ok_or_else(|| {
                    Error::new(ErrorCode::AuthFailed, t!("provider.postgres.connect.no_entra_token"))
                        .with_hint(t!("provider.postgres.connect.no_entra_token_hint"))
                })?;
                config.password(token.as_bytes());
            }
            AuthKind::AwsIam => {
                if !aws_iam_host_allowed(&spec.host) {
                    return Err(Error::new(ErrorCode::PermissionDenied, t!("provider.postgres.connect.iam_host")));
                }
                let token = spec.token.as_ref().ok_or_else(|| {
                    Error::new(ErrorCode::AuthFailed, t!("provider.postgres.connect.no_iam_token"))
                        .with_hint(t!("provider.postgres.connect.no_iam_token_hint"))
                })?;
                config.password(token.as_bytes());
            }
            AuthKind::None => {}
        }
        let (ssl_mode, tls) = tls_for(spec)?;
        config.ssl_mode(ssl_mode);
        Ok(Connector {
            fence: culprit_read::ReadFence::default(),
            config,
            tls,
            transport: spec.transport.clone(),
            host: spec.host.trim().to_string(),
            port,
        })
    }

    async fn dial_config(&self) -> Result<(tokio_postgres::Config, Option<ConnectionEndpoint>)> {
        self.fence.ensure_available()?;
        let mut config = self.config.clone();
        let endpoint = match &self.transport {
            Some(transport) => {
                let address = transport.endpoint(&self.host, self.port).await?;
                config.hostaddr(address.address.ip());
                Some(address)
            }
            None => None,
        };
        config.port(endpoint.as_ref().map_or(self.port, |endpoint| endpoint.address.port()));
        Ok((config, endpoint))
    }

    pub(crate) async fn cancel(&self, token: &tokio_postgres::CancelToken) -> Result<()> {
        token.cancel_query(self.tls.clone()).await.map_err(map_error)
    }

    /// Open a session with Culprit's safety settings applied: read-only by default, bounded statement and lock waits.
    pub async fn connect(&self, limits: SessionLimits) -> Result<Client> {
        Ok(self.connect_observed(limits).await?.client)
    }

    pub async fn connect_observed(&self, limits: SessionLimits) -> Result<ObservedSession> {
        let session = self.connect_unconfigured().await?;
        session.client.batch_execute(&session_setup(limits)).await.map_err(map_error)?;
        Ok(session)
    }

    /// Only the owned native reader may defer setup; it runs these commands under the same cancellation owner
    /// as every subsequent statistics query before returning any caller-requested data.
    pub(crate) async fn connect_unconfigured(&self) -> Result<ObservedSession> {
        let (config, endpoint) = self.dial_config().await?;
        let (client, mut connection) = tokio::time::timeout(CONNECT_TIMEOUT, config.connect(self.tls.clone()))
            .await
            .map_err(|_| {
                Error::new(
                    ErrorCode::Timeout,
                    t!("provider.postgres.connect.timed_out", host = &self.host, port = self.port.to_string()),
                )
            })?
            .map_err(|error| transport_error(endpoint.as_ref(), map_connect_error(error, &self.host, self.port)))?;
        let notices = Arc::new(AtomicU64::new(0));
        let observed = notices.clone();
        tokio::spawn(async move {
            let _endpoint = endpoint;
            while let Some(message) = futures_util::future::poll_fn(|cx| connection.poll_message(cx)).await {
                match message {
                    Ok(AsyncMessage::Notice(_)) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        });
        Ok(ObservedSession { client, notices })
    }
}

pub(crate) fn session_setup(limits: SessionLimits) -> String {
    let mut setup = vec![
        "SET default_transaction_read_only = on".to_string(),
        format!("SET statement_timeout = '{}ms'", limits.statement_timeout.as_millis()),
        "SET lock_timeout = '2s'".to_string(),
        "SET idle_in_transaction_session_timeout = '30s'".to_string(),
    ];
    if limits.catalog_path {
        setup.push("SET search_path = pg_catalog, pg_temp".to_string());
    }
    setup.iter().map(|statement| crate::rows::tagged(statement)).collect::<Vec<_>>().join("; ")
}

fn tls_for(spec: &ConnectionSpec) -> Result<(SslMode, MakeRustlsConnect)> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    // Entra and IAM tokens are bearer credentials: never send them over unverified TLS.
    let security =
        culprit_core::Engine::Postgres.spec().connection_security().expect("PostgreSQL has network security settings");
    let default = security.describe(spec).default_mode;
    let mode = if matches!(spec.auth, AuthKind::Entra | AuthKind::AwsIam) {
        default
    } else {
        spec.option("sslmode").unwrap_or(&default).to_ascii_lowercase()
    };
    let root_cert = spec.option("sslrootcert").filter(|p| !p.is_empty() && *p != "system");
    let (ssl_mode, verification) = match mode.as_str() {
        "disable" => (SslMode::Disable, Verification::None),
        "allow" | "prefer" => (SslMode::Prefer, Verification::None),
        // libpq: `require` with a root certificate behaves like `verify-ca`.
        "require" if root_cert.is_some() => (SslMode::Require, Verification::Chain),
        "require" => (SslMode::Require, Verification::None),
        "verify-ca" => (SslMode::Require, Verification::Chain),
        "verify-full" => (SslMode::Require, Verification::Full),
        other => {
            return Err(Error::invalid(t!("provider.postgres.connect.unknown_sslmode", mode = other))
                .with_hint(t!("provider.postgres.connect.unknown_sslmode_hint")));
        }
    };
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::internal(t!("provider.postgres.connect.tls_setup_failed", error = e.to_string())))?;
    let config = match verification {
        Verification::None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
            .with_no_client_auth(),
        Verification::Chain => {
            let inner =
                WebPkiServerVerifier::builder_with_provider(Arc::new(roots(root_cert)?), provider).build().map_err(
                    |e| Error::internal(t!("provider.postgres.connect.tls_setup_failed", error = e.to_string())),
                )?;
            builder.dangerous().with_custom_certificate_verifier(Arc::new(IgnoreHostname(inner))).with_no_client_auth()
        }
        Verification::Full => builder.with_root_certificates(roots(root_cert)?).with_no_client_auth(),
    };
    Ok((ssl_mode, MakeRustlsConnect::new(config)))
}

enum Verification {
    None,
    Chain,
    Full,
}

fn roots(extra_pem: Option<&str>) -> Result<RootCertStore> {
    let mut store = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let Some(path) = extra_pem else { return Ok(store) };
    let certs = CertificateDer::pem_file_iter(path).map_err(|e| {
        Error::invalid(t!("provider.postgres.connect.unreadable_root_cert", path = path, error = e.to_string()))
    })?;
    let invalid =
        |e: String| Error::invalid(t!("provider.postgres.connect.invalid_certificate", path = path, error = e));
    for cert in certs {
        let cert = cert.map_err(|e| invalid(e.to_string()))?;
        store.add(cert).map_err(|e| invalid(e.to_string()))?;
    }
    Ok(store)
}

/// libpq's `prefer` / `require`: encrypt the connection without authenticating the server.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// libpq's `verify-ca`: the chain must lead to a trusted root, the host name is not checked.
#[derive(Debug)]
struct IgnoreHostname(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for IgnoreHostname {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        match self.0.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

fn io_kind(error: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        current = e.source();
    }
    None
}

fn full_message(error: &tokio_postgres::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(e) = source {
        message.push_str(": ");
        message.push_str(&e.to_string());
        source = e.source();
    }
    message
}

fn map_connect_error(error: tokio_postgres::Error, host: &str, port: u16) -> Error {
    if error.as_db_error().is_some() {
        return map_error(error);
    }
    let message = full_message(&error);
    let lower = message.to_ascii_lowercase();
    let port = port.to_string();
    let timed_out = || {
        Error::new(ErrorCode::Timeout, t!("provider.postgres.connect.timed_out", host = host, port = &port))
            .with_hint(t!("provider.postgres.connect.timed_out_hint"))
    };
    match io_kind(&error) {
        Some(std::io::ErrorKind::ConnectionRefused) => {
            return Error::new(
                ErrorCode::ConnectionFailed,
                t!("provider.postgres.connect.refused", host = host, port = &port),
            )
            .with_hint(t!("provider.postgres.connect.refused_hint"));
        }
        Some(std::io::ErrorKind::TimedOut) => return timed_out(),
        _ => {}
    }
    if lower.contains("timeout") {
        return timed_out();
    }
    if lower.contains("invalid peer certificate") {
        return Error::new(ErrorCode::ConnectionFailed, message)
            .with_hint(t!("provider.postgres.connect.untrusted_certificate_hint"))
            .with_detail(culprit_core::ErrorDetail::ConnectionSecurity);
    }
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("ssl") {
        return Error::new(ErrorCode::ConnectionFailed, message)
            .with_hint(t!("provider.postgres.connect.tls_failed_hint"))
            .with_detail(culprit_core::ErrorDetail::ConnectionSecurity);
    }
    if lower.contains("failed to lookup")
        || lower.contains("nodename nor servname")
        || lower.contains("name or service not known")
    {
        return Error::new(ErrorCode::ConnectionFailed, t!("provider.postgres.connect.unresolved_host", host = host))
            .with_hint(t!("provider.postgres.connect.unresolved_host_hint"));
    }
    if lower.contains("password missing") {
        return Error::new(ErrorCode::AuthFailed, t!("provider.postgres.connect.no_password"))
            .with_hint(t!("provider.postgres.connect.no_password_hint"));
    }
    if lower.contains("authentication") {
        return Error::new(ErrorCode::AuthFailed, message)
            .with_hint(t!("provider.postgres.connect.rejected_password_hint"));
    }
    Error::new(ErrorCode::ConnectionFailed, message)
}

pub fn map_error(error: tokio_postgres::Error) -> Error {
    let Some(db) = error.as_db_error() else {
        let message = full_message(&error);
        if error.is_closed() {
            return Error::new(ErrorCode::ConnectionFailed, t!("provider.postgres.connect.closed"));
        }
        return Error::new(ErrorCode::QueryFailed, message);
    };
    let mut message = db.message().to_string();
    if let Some(detail) = db.detail() {
        message.push_str(&format!(" ({detail})"));
    }
    let code = db.code().code();
    match code {
        "28P01" | "28000" => {
            Error::new(ErrorCode::AuthFailed, message).with_hint(t!("provider.postgres.error.auth_hint"))
        }
        "42501" => {
            Error::new(ErrorCode::PermissionDenied, message).with_hint(t!("provider.postgres.error.permission_hint"))
        }
        "57014" => Error::new(ErrorCode::Timeout, message).with_hint(t!("provider.postgres.error.timeout_hint")),
        "55P03" => Error::new(ErrorCode::Timeout, message).with_hint(t!("provider.postgres.error.lock_timeout_hint")),
        "3D000" => Error::new(ErrorCode::NotFound, message).with_hint(t!("provider.postgres.error.database_hint")),
        "53300" => {
            Error::new(ErrorCode::ConnectionFailed, message).with_hint(t!("provider.postgres.error.no_slots_hint"))
        }
        "25006" => {
            Error::new(ErrorCode::PermissionDenied, message).with_hint(t!("provider.postgres.error.read_only_hint"))
        }
        "42601" | "42P01" | "42703" | "42883" | "42P02" => {
            let hint = db.hint().map(str::to_string);
            let e = Error::new(ErrorCode::InvalidInput, message);
            match hint {
                Some(h) => e.with_hint(h),
                None => e,
            }
        }
        _ => {
            let hint = db.hint().map(str::to_string);
            let e = Error::new(ErrorCode::QueryFailed, format!("{message} [{code}]"));
            match hint {
                Some(h) => e.with_hint(h),
                None => e,
            }
        }
    }
}

pub fn sql_state(error: &tokio_postgres::Error) -> Option<&str> {
    error.as_db_error().map(|db| db.code().code())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Route;
    #[async_trait::async_trait]
    impl ConnectionTransport for Route {
        async fn endpoint(&self, host: &str, port: u16) -> Result<ConnectionEndpoint> {
            assert_eq!((host, port), ("shop.abc.us-east-1.rds.amazonaws.com", 5432));
            Ok(ConnectionEndpoint::new("127.0.0.1:15432".parse().unwrap()))
        }
    }

    #[tokio::test]
    async fn routed_tcp_keeps_the_remote_tls_and_iam_identity() {
        let spec = ConnectionSpec {
            host: "shop.abc.us-east-1.rds.amazonaws.com".into(),
            auth: AuthKind::AwsIam,
            token: Some("short-lived-token".into()),
            transport: Some(Arc::new(Route)),
            ..Default::default()
        };
        let connector = Connector::new(&spec).unwrap();
        let (config, _) = connector.dial_config().await.unwrap();
        assert_eq!(config.get_hosts(), &[tokio_postgres::config::Host::Tcp(spec.host)]);
        assert_eq!(config.get_hostaddrs(), &["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]);
        assert_eq!(config.get_ports(), &[15432]);
        assert!(matches!(config.get_ssl_mode(), SslMode::Require));
        assert_eq!(connector.port, 5432);
    }

    fn spec(sslmode: Option<&str>) -> ConnectionSpec {
        let mut spec = ConnectionSpec { host: "db.example.com".into(), ..Default::default() };
        if let Some(mode) = sslmode {
            spec.options.insert("sslmode".into(), mode.into());
        }
        spec
    }

    #[test]
    fn sslmodes_follow_libpq() {
        // No sslmode: a remote host must present a verifying certificate; a local one stays opportunistic.
        assert!(matches!(tls_for(&spec(None)).unwrap().0, SslMode::Require));
        let local = ConnectionSpec { host: "localhost".into(), ..Default::default() };
        assert!(matches!(tls_for(&local).unwrap().0, SslMode::Prefer));
        assert!(matches!(tls_for(&spec(Some("disable"))).unwrap().0, SslMode::Disable));
        assert!(matches!(tls_for(&spec(Some("require"))).unwrap().0, SslMode::Require));
        assert!(matches!(tls_for(&spec(Some("verify-full"))).unwrap().0, SslMode::Require));
        assert!(tls_for(&spec(Some("bogus"))).is_err());
    }

    #[test]
    fn entra_without_token_is_an_auth_error() {
        let mut s = spec(None);
        s.auth = AuthKind::Entra;
        s.host = "shop.postgres.database.azure.com".into();
        let error = Connector::new(&s).err().unwrap();
        assert_eq!(error.code, ErrorCode::AuthFailed);
    }

    #[test]
    fn iam_tokens_stay_on_verified_rds_hosts() {
        let mut s = spec(Some("disable"));
        s.auth = AuthKind::AwsIam;
        s.token = Some("t".into());
        s.host = "collector.attacker.example".into();
        assert_eq!(Connector::new(&s).err().unwrap().code, ErrorCode::PermissionDenied);
        s.host = "shop.abc123.us-east-1.rds.amazonaws.com".into();
        assert!(matches!(tls_for(&s).unwrap().0, SslMode::Require));
        s.token = None;
        assert_eq!(Connector::new(&s).err().unwrap().code, ErrorCode::AuthFailed);
    }

    #[test]
    fn entra_tokens_stay_on_verified_azure_hosts() {
        let mut s = spec(Some("disable"));
        s.auth = AuthKind::Entra;
        s.token = Some("t".into());
        s.host = "collector.attacker.example".into();
        assert_eq!(Connector::new(&s).err().unwrap().code, ErrorCode::PermissionDenied);
        s.host = "shop.postgres.database.azure.com".into();
        assert!(matches!(tls_for(&s).unwrap().0, SslMode::Require));
    }

    fn refused(spec: &ConnectionSpec) -> (String, Option<String>) {
        let error = Connector::new(spec).err().unwrap();
        (error.message, error.hint)
    }

    // Why a connection can't be opened, word for word.
    #[test]
    fn connection_errors_in_english() {
        assert_eq!(
            refused(&ConnectionSpec { host: " ".into(), ..Default::default() }),
            ("A database connection needs a host".into(), None)
        );
        let mut s = spec(None);
        s.auth = AuthKind::Entra;
        assert_eq!(
            refused(&s),
            ("Microsoft Entra ID tokens are only sent to Azure Database for PostgreSQL hosts".into(), None)
        );
        s.host = "shop.postgres.database.azure.com".into();
        assert_eq!(
            refused(&s),
            (
                "No Microsoft Entra token was resolved for this connection".into(),
                Some("Sign in with `az login`, then try again.".into())
            )
        );
        s.auth = AuthKind::AwsIam;
        assert_eq!(refused(&s), ("AWS IAM authentication tokens are only sent to Amazon RDS hosts".into(), None));
        s.host = "shop.abc123.us-east-1.rds.amazonaws.com".into();
        assert_eq!(
            refused(&s),
            (
                "No AWS IAM token was resolved for this connection".into(),
                Some("Sign in with `aws login`, then try again.".into())
            )
        );
        assert_eq!(
            refused(&spec(Some("bogus"))),
            (
                "Unknown sslmode `bogus`".into(),
                Some("Use one of disable, prefer, require, verify-ca, verify-full.".into())
            )
        );
        let mut s = spec(Some("verify-full"));
        s.options.insert("sslrootcert".into(), "/nonexistent/culprit-ca.pem".into());
        let (message, hint) = refused(&s);
        assert!(message.starts_with("Cannot read sslrootcert `/nonexistent/culprit-ca.pem`: "), "{message}");
        assert_eq!(hint, None);
    }
}
