//! The plain data the workspace takes and returns: connections as the app lists them, a new connection, a pasted
//! connection string read back, and the request and answer for a period's events.

use culprit_core::{
    AuthKind, ConnectionSecurity, ConnectionSecurityInfo, Engine, Environment, Error, Result, SshConfig, t,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{AgentAccess, ReportSummary};

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConnectionInfo {
    pub name: String,
    pub engine: Engine,
    pub environment: Environment,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub auth: AuthKind,
    /// A password is available (keychain or environment variable). Only meaningful for password authentication.
    pub has_secret: bool,
    pub display: String,
    pub last_report: Option<ReportSummary>,
    /// What coding agents may do with this connection through `culprit mcp`.
    pub agent_access: AgentAccess,
    /// The application's query log scans read the statements from when the database keeps none (SQLite).
    pub query_log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ssh: Option<SshConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub security: Option<ConnectionSecurityInfo>,
}

/// A period on a connection's timeline. Everything is optional: by default the last 7 days.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct EventsRequest {
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    /// The last N hours, instead of `start`.
    #[serde(default)]
    pub hours: Option<f64>,
    /// Read cloud metrics too (default: yes, for Amazon RDS and Azure databases).
    #[serde(default)]
    pub cloud: Option<bool>,
    /// Fetch cloud metrics again instead of using the last few minutes' answer.
    #[serde(default)]
    pub refresh: bool,
}

impl EventsRequest {
    /// `(start, end)` in epoch seconds.
    pub fn range(&self) -> Result<(f64, f64)> {
        let parse = |field: &str, value: &Option<String>| -> Result<Option<f64>> {
            value
                .as_deref()
                .map(|v| {
                    culprit_core::changes::epoch(v).ok_or_else(|| {
                        Error::invalid(t!("engine.types.bad_timestamp", field = field, value = v))
                            .with_hint(t!("engine.types.timestamp_hint"))
                    })
                })
                .transpose()
        };
        let now = chrono::Utc::now().timestamp() as f64;
        let end = parse("end", &self.end)?.unwrap_or(now).min(now);
        let start = match (parse("start", &self.start)?, self.hours.filter(|h| *h > 0.0)) {
            (Some(start), _) => start,
            (None, Some(hours)) => end - hours * 3600.0,
            (None, None) => end - 7.0 * 86_400.0,
        };
        if start >= end {
            return Err(Error::invalid(t!("engine.types.empty_period")));
        }
        Ok((start, end))
    }
}

/// What changed around a connection over a period.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct EventLog {
    pub connection: String,
    pub start: String,
    pub end: String,
    pub events: Vec<culprit_core::ChangeEvent>,
    /// Why something is missing (cloud metrics unavailable, not signed in).
    pub notes: Vec<String>,
}

/// What the UI and CLI send to save a connection. `input` is anything `connstr::parse` understands; explicit fields win over it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default)]
#[ts(export)]
pub struct NewConnection {
    pub name: String,
    pub input: String,
    pub environment: Environment,
    pub engine: Option<Engine>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    /// Read the password from this environment variable at connect time instead of storing it.
    pub password_env: Option<String>,
    pub auth: Option<AuthKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ssh: Option<SshConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub security: Option<ConnectionSecurity>,
}

/// What a chosen database file is as a connection's input: its path when the extension says SQLite (`.db`, `.sqlite`,
/// `.sqlite3`, `.db3`), else `sqlite:<path>`, so Culprit reads it as SQLite whatever the file is named. The app's file
/// picker hands this to the new-connection form.
pub fn database_file_input(path: &std::path::Path) -> String {
    crate::connection::database_file_input(path)
}

/// The non-secret result of parsing a pasted connection string, for live feedback while typing.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ParsedConnection {
    pub engine: Option<Engine>,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub auth: AuthKind,
    pub has_password: bool,
    pub display: String,
    pub suggested_name: String,
    pub environment: Environment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub security: Option<ConnectionSecurityInfo>,
}

/// Editable metadata. Endpoints, credentials, SSH routes and report ownership are retained.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConnectionSettings {
    pub environment: Environment,
    pub security: ConnectionSecurity,
}
