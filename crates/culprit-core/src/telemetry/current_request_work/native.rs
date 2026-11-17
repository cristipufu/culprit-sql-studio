//! Closed source-owned request details keep native identity and counter meanings independent.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CurrentRequestWorkProfile {
    MysqlStatementEvent,
    PostgresCopy,
}

impl CurrentRequestWorkProfile {
    pub const ALL: [Self; 2] = [Self::MysqlStatementEvent, Self::PostgresCopy];

    pub fn engine(self) -> Engine {
        match self {
            Self::MysqlStatementEvent => Engine::Mysql,
            Self::PostgresCopy => Engine::Postgres,
        }
    }

    pub fn matches_identity(self, actor: &ActivityKey, scope: ActivityScopeBasis, statement: &NativeIdentity) -> bool {
        let Some(request) = &actor.request else { return false };
        match self {
            Self::MysqlStatementEvent => {
                matches!(scope, ActivityScopeBasis::RequestDatabase | ActivityScopeBasis::SessionDatabase)
                    && crate::engines::mysql_activity_statement_identity(&actor.session, request).as_ref()
                        == Some(statement)
            }
            Self::PostgresCopy => {
                scope == ActivityScopeBasis::SessionDatabase
                    && postgres_actor_is_valid(actor)
                    && crate::engines::postgres_activity_statement_identity(
                        &actor.session.id,
                        &actor.session.generation,
                        &request.generation,
                    ) == *statement
            }
        }
    }

    pub fn accepts_focus(self, focus: &ActivityRequestFocus) -> bool {
        focus.is_valid()
            && focus.target.engine() == self.engine()
            && self.matches_identity(&focus.original.key, focus.original.scope, &focus.statement_identity)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum CurrentRequestWorkNative {
    Mysql { work: Box<MysqlCurrentRequestWork> },
    PostgresCopy { progress: Box<PostgresCopyProgress> },
}

impl CurrentRequestWorkNative {
    pub fn profile(&self) -> CurrentRequestWorkProfile {
        match self {
            Self::Mysql { .. } => CurrentRequestWorkProfile::MysqlStatementEvent,
            Self::PostgresCopy { .. } => CurrentRequestWorkProfile::PostgresCopy,
        }
    }
    pub fn engine(&self) -> Engine {
        self.profile().engine()
    }

    pub fn mysql(&self) -> Option<&MysqlCurrentRequestWork> {
        match self {
            Self::Mysql { work } => Some(work),
            Self::PostgresCopy { .. } => None,
        }
    }

    pub fn mysql_mut(&mut self) -> Option<&mut MysqlCurrentRequestWork> {
        match self {
            Self::Mysql { work } => Some(work),
            Self::PostgresCopy { .. } => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::Mysql { work } => work.is_valid(),
            Self::PostgresCopy { progress } => progress.is_valid(),
        }
    }

    pub(super) fn matches_embedded_actor(&self, actor: &ActivityKey) -> bool {
        match self {
            Self::Mysql { work } => work.matches_actor(actor),
            Self::PostgresCopy { .. } => true,
        }
    }
}

fn postgres_actor_is_valid(actor: &ActivityKey) -> bool {
    actor
        .session
        .id
        .parse::<u32>()
        .is_ok_and(|pid| pid > 0 && pid <= i32::MAX as u32 && pid.to_string() == actor.session.id)
        && canonical_time(&actor.session.generation).is_ok()
        && actor
            .request
            .as_ref()
            .is_some_and(|request| request.id == "statement" && canonical_time(&request.generation).is_ok())
        && actor.transaction.is_none()
}
