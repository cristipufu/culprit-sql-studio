use super::*;

/// Existing performance_schema.threads metadata. Default database is a mutable session setting, not request schema.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlSessionContext {
    #[serde(deserialize_with = "Option::deserialize")]
    pub default_database: Option<String>,
    /// Native CURRENT_SCHEMA only for a bound current event, never an idle row's retained previous event.
    #[serde(deserialize_with = "Option::deserialize")]
    pub request_schema: Option<String>,
    /// Original TYPE; a thread identifier alone does not establish a client participant.
    #[serde(deserialize_with = "Option::deserialize")]
    pub thread_type: Option<String>,
    /// Original server instrument NAME, independent of client program/login labels.
    #[serde(deserialize_with = "Option::deserialize")]
    pub thread_name: Option<String>,
}
impl MysqlSessionContext {
    pub fn lifecycle(idle: bool, current_request: bool) -> SessionLifecycle {
        if idle {
            SessionLifecycle::Idle
        } else if current_request {
            SessionLifecycle::Running
        } else {
            SessionLifecycle::Unknown
        }
    }
    pub fn is_client_thread(&self, command: Option<&str>) -> bool {
        self.thread_type.as_deref() == Some("FOREGROUND")
            && self.thread_name.as_deref() == Some("thread/sql/one_connection")
            && matches!(command, Some("Query" | "Execute" | "Sleep"))
    }
    pub(super) fn is_valid_for(&self, context: &ActivitySessionContext, key: &ActivityKey, idle: bool) -> bool {
        self.default_database
            .as_deref()
            .is_none_or(|value| !value.is_empty() && value.len() <= MAX_NATIVE_DATABASE_NAME_BYTES)
            && self.request_schema.as_deref().is_none_or(|value| {
                key.request.is_some() && !value.is_empty() && value.len() <= MAX_NATIVE_DATABASE_NAME_BYTES
            })
            && self.thread_type.as_deref().is_none_or(identifier)
            && self.thread_name.as_deref().is_none_or(identifier)
            && positive(&key.session.id)
            && positive(&key.session.generation)
            && key.request.as_ref().is_none_or(|request| {
                native_unsigned(&request.id).is_some() && native_unsigned(&request.generation).is_some()
            })
            && context.lifecycle == Self::lifecycle(idle, key.request.is_some())
    }
}
fn positive(value: &str) -> bool {
    native_unsigned(value).is_some_and(|value| value > 0)
}

fn native_unsigned(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| number.to_string() == value)
}
