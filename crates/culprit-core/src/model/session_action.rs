//! Administrative signals retain their observed source and native generations. Native PID/SPID signalling is
//! still a precheck followed by a signal, not an atomic compare-and-signal operation or proof of completion.
use super::*;
use crate::telemetry::{ActivityKey, NativeIdentity, NativeReadContext};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SessionActionTarget {
    pub context: NativeReadContext,
    pub activity: ActivityKey,
    pub statement: Option<NativeIdentity>,
}

impl SessionActionTarget {
    pub fn is_valid_for(&self, action: SessionAction) -> bool {
        self.context.has_qualified_local_owner()
            && matches!(self.context.target, crate::telemetry::CollectionTarget::Database(_))
            && self.activity.is_valid()
            && self.statement.as_ref().is_none_or(NativeIdentity::is_valid)
            && (action != SessionAction::Cancel || self.activity.request.is_some())
    }

    /// Observation bounds advance on every read; they are provenance, not a new session generation.
    /// A session-level termination remains scoped to that session when its current request advances.
    pub fn matches_action(&self, current: &Self, action: SessionAction) -> bool {
        self.is_valid_for(action)
            && current.is_valid_for(action)
            && self.context.target == current.context.target
            && self.context.partition == current.context.partition
            && self.context.database_writable == current.context.database_writable
            && self.activity.session == current.activity.session
            && (action != SessionAction::Cancel
                || (self.activity.request == current.activity.request && self.statement == current.statement))
    }

    pub fn require_current(&self, current: &Self, action: SessionAction) -> crate::Result<()> {
        if self.matches_action(current, action) { Ok(()) } else { Err(Self::changed()) }
    }

    pub fn changed() -> crate::Error {
        crate::Error::invalid(crate::t!("model.session_action.changed"))
            .with_hint(crate::t!("model.session_action.changed_hint"))
            .with_detail(crate::ErrorDetail::SessionActionChanged)
    }

    pub fn unavailable() -> crate::Error {
        crate::Error::new(crate::ErrorCode::Unsupported, crate::t!("model.session_action.unavailable"))
    }

    pub fn production() -> crate::Error {
        crate::Error::new(crate::ErrorCode::PermissionDenied, crate::t!("model.session_action.production"))
    }

    pub fn signal_failed() -> crate::Error {
        crate::Error::new(crate::ErrorCode::QueryFailed, crate::t!("model.session_action.signal_failed"))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SessionActionRoute {
    pub connection: String,
    pub revision: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SessionActionRequest {
    pub route: SessionActionRoute,
    pub target: SessionActionTarget,
    pub action: SessionAction,
}

impl SessionActionRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.route.connection.is_empty()
            || self.route.connection.len() > 64
            || !crate::telemetry::valid_digest(&self.route.revision)
            || !self.target.is_valid_for(self.action)
        {
            return Err(SessionActionTarget::unavailable());
        }
        let engine = self.target.context.target.engine();
        let session = engine.spec().live_session_id(&self.target.activity.session);
        engine.spec().validate_session_id(&session)?;
        Ok(())
    }
}

impl LiveState {
    /// Preserve one exact observation for a later signal. Ambiguous IDs cannot select an arbitrary row.
    pub fn action_request(&self, session: &str, action: SessionAction) -> crate::Result<SessionActionRequest> {
        if !self.session_actions.contains(&action) {
            return Err(SessionActionTarget::unavailable());
        }
        let mut rows = self.sessions.iter().filter(|row| row.id == session);
        let row = rows.next().ok_or_else(SessionActionTarget::changed)?;
        if rows.next().is_some() {
            return Err(SessionActionTarget::changed());
        }
        let request = SessionActionRequest {
            route: self.session_action_route.clone().ok_or_else(SessionActionTarget::unavailable)?,
            target: row.action_target.clone().ok_or_else(SessionActionTarget::unavailable)?,
            action,
        };
        request.validate()?;
        let engine = request.target.context.target.engine();
        if engine.spec().live_session_id(&request.target.activity.session) != row.id {
            return Err(SessionActionTarget::changed());
        }
        Ok(request)
    }
}

/// The native server accepted the administrative signal. It has not confirmed cancellation, exit or rollback.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SessionActionOutcome {
    SignalSent,
}

#[cfg(test)]
mod tests;
