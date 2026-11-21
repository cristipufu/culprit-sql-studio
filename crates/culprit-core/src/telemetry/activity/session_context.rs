//! Session metadata is context. Its lifetimes, client claims and connection tokens do not identify a request owner.
use super::*;

const MAX_NATIVE_DATABASE_NAME_BYTES: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SessionLifecycle {
    Running,
    Idle,
    PoolResetPrelogin,
    Connecting,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivitySessionContext {
    pub lifecycle: SessionLifecycle,
    /// Difference of the same-read native sample and login clocks; not current-request duration or a reset epoch.
    pub login_age_ms: Option<String>,
    pub last_request_end_age_ms: Option<String>,
    pub client_library: Option<String>,
    /// Client-reported process identity; never the database server process or an authenticated application identity.
    pub client_process_id: Option<String>,
    pub native: Option<Box<SessionNativeContext>>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum SessionNativeContext {
    Mssql(MssqlSessionContext),
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlSessionTimeTokens {
    pub login_without_offset: String,
    /// Includes a currently executing request; it is not a binding to one MARS request's statement.
    pub last_request_start_without_offset: Option<String>,
    /// The most recently completed request can differ from both the current request and the latest start.
    pub last_request_end_without_offset: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlSessionContext {
    pub native_state: String,
    /// The native 1900-01-01 prelogin sentinel cannot identify an established session generation.
    pub login_established: bool,
    pub tds_protocol_version: Option<String>,
    /// Signed native sys.dm_exec_sessions.total_elapsed_time, in milliseconds since session establishment.
    /// Negative values remain raw facts but cannot supply a nonnegative age. This is not request work or CPU.
    #[serde(deserialize_with = "Option::deserialize")]
    pub elapsed_ms: Option<String>,
    /// Original sys.dm_exec_sessions database_id, scoped to this native source and point; not a global identity.
    /// A reported zero is retained separately from an unacquired value and supplies no database association.
    pub session_database_id: Option<String>,
    /// Optional DB_NAME(session_database_id) label. NULL/zero never fall back to the reader's database.
    pub session_database_name: Option<String>,
    /// Original request-row database_id, independent of statement text/locators and the session's database.
    pub request_database_id: Option<String>,
    pub times: MssqlSessionTimeTokens,
    pub has_current_request: bool,
    /// Taken only from this exact native request row. No representative connection is selected by session ID.
    pub request_connection_id: Option<String>,
}
impl MssqlSessionContext {
    fn database_context_is_valid(&self, has_request: bool) -> bool {
        self.session_database_id.as_deref().is_none_or(native_database_id)
            && self.session_database_name.as_deref().is_none_or(|name| {
                !name.is_empty()
                    && name.len() <= MAX_NATIVE_DATABASE_NAME_BYTES
                    && self.session_database_id.as_deref().is_some_and(|id| id != "0")
            })
            && self.request_database_id.as_deref().is_none_or(|id| has_request && native_database_id(id))
    }

    /// Native sys.dm_exec_sessions status only; current request status is a separate observation.
    pub fn lifecycle_from_native_state(native_state: &str) -> SessionLifecycle {
        match native_state.to_ascii_lowercase().as_str() {
            "running" => SessionLifecycle::Running,
            "sleeping" => SessionLifecycle::Idle,
            "dormant" => SessionLifecycle::PoolResetPrelogin,
            "preconnect" => SessionLifecycle::Connecting,
            _ => SessionLifecycle::Unknown,
        }
    }

    /// Native pool-reset/prelogin rows cannot supply an established session generation or current request.
    pub fn login_is_established(login_without_offset: &str, lifecycle: SessionLifecycle) -> bool {
        !login_without_offset.starts_with("1900-01-01T")
            && !matches!(lifecycle, SessionLifecycle::PoolResetPrelogin | SessionLifecycle::Connecting)
    }
}

impl ActivitySessionContext {
    pub fn is_valid_for(&self, key: &ActivityKey, idle: bool) -> bool {
        self.login_age_ms.as_deref().is_none_or(counter)
            && self.last_request_end_age_ms.as_deref().is_none_or(counter)
            && self.client_process_id.as_deref().is_none_or(identifier)
            && self.client_library.as_deref().is_none_or(|value| value.len() <= 512)
            && self.native.as_deref().is_none_or(|native| match native {
                SessionNativeContext::Mssql(context) => {
                    identifier(&context.native_state)
                        && self.lifecycle == MssqlSessionContext::lifecycle_from_native_state(&context.native_state)
                        && context.tds_protocol_version.as_deref().is_none_or(native_integer_code)
                        && context.elapsed_ms.as_deref().is_none_or(native_integer_code)
                        && context.database_context_is_valid(key.request.is_some())
                        && self.client_process_id.as_deref().is_none_or(native_integer_code)
                        && context.times.is_valid()
                        && context.times.login_without_offset == key.session.generation
                        && context.login_established
                            == MssqlSessionContext::login_is_established(
                                &context.times.login_without_offset,
                                self.lifecycle,
                            )
                        && (context.login_established || self.login_age_ms.is_none())
                        && (self.last_request_end_age_ms.is_none()
                            || context.times.last_request_end_without_offset.is_some())
                        && (context.login_established || key.request.is_none())
                        && idle != context.has_current_request
                        && (key.request.is_none() || context.has_current_request)
                        && context
                            .request_connection_id
                            .as_deref()
                            .is_none_or(|id| key.request.is_some() && native_connection_token(id))
                }
            })
    }
}

impl MssqlSessionTimeTokens {
    pub fn is_valid(&self) -> bool {
        native_local_time(&self.login_without_offset)
            && self.last_request_start_without_offset.as_deref().is_none_or(native_local_time)
            && self.last_request_end_without_offset.as_deref().is_none_or(native_local_time)
    }
}

fn native_local_time(value: &str) -> bool {
    // The native datetime2 projection has at most seven fractional digits. Bounded parsing cannot truncate
    // excess precision or accept an offset that the source never supplied.
    (19..=27).contains(&value.len()) && chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
}

fn native_connection_token(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
        })
}

fn native_database_id(value: &str) -> bool {
    value.parse::<i16>().is_ok_and(|native| native >= 0 && native.to_string() == value)
}

fn native_integer_code(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|native| native.to_string() == value)
}

#[cfg(test)]
mod tests;
