use super::*;

/// Already-acquired pg_stat_activity backend metadata. Database OID is bound to this backend's native row.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresSessionContext {
    #[serde(deserialize_with = "Option::deserialize")]
    pub database_oid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub database_name: Option<String>,
    /// Original backend_type; active state does not distinguish clients from parallel/internal workers.
    #[serde(deserialize_with = "Option::deserialize")]
    pub backend_type: Option<String>,
}
impl PostgresSessionContext {
    pub fn lifecycle(idle: bool) -> SessionLifecycle {
        if idle { SessionLifecycle::Idle } else { SessionLifecycle::Running }
    }
    pub fn is_client_backend(&self) -> bool {
        self.backend_type.as_deref() == Some("client backend")
    }
    pub(super) fn is_valid_for(&self, context: &ActivitySessionContext, key: &ActivityKey, idle: bool) -> bool {
        self.database_oid
            .as_deref()
            .is_none_or(|value| value.parse::<u32>().is_ok_and(|value_id| value_id.to_string() == value))
            && self
                .database_name
                .as_deref()
                .is_none_or(|value| !value.is_empty() && value.len() <= MAX_NATIVE_DATABASE_NAME_BYTES)
            && self.backend_type.as_deref().is_none_or(identifier)
            && super::super::super::identity::valid_utc_instant(&key.session.generation)
            && key.request.as_ref().is_none_or(|request| {
                request.id == "statement" && super::super::super::identity::valid_utc_instant(&request.generation)
            })
            && context.lifecycle == Self::lifecycle(idle)
    }
}
