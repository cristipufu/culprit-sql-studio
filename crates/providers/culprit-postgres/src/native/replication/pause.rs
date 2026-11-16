//! Optional recovery functions must be admitted before constructing the native statement.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplayPauseAccess {
    pub requested: bool,
    pub state: bool,
}

impl ReplayPauseAccess {
    // Resolve optional functions by OID: unsupported versions return NULL without binding a missing callable.
    // https://www.postgresql.org/docs/18/functions-info.html#FUNCTIONS-INFO-CATALOG-TABLE
    pub(crate) const PROJECTION: &str = "coalesce(pg_catalog.has_function_privilege(current_user,pg_catalog.to_regprocedure('pg_catalog.pg_is_wal_replay_paused()'),'EXECUTE'),false) AS replay_pause_requested_access,\
         coalesce(pg_catalog.has_function_privilege(current_user,pg_catalog.to_regprocedure('pg_catalog.pg_get_wal_replay_pause_state()'),'EXECUTE'),false) AS replay_pause_state_access";

    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            requested: flag(row, "replay_pause_requested_access")?,
            state: flag(row, "replay_pause_state_access")?,
        })
    }

    pub(super) fn projection(self, version: u32) -> String {
        // PostgreSQL initializes every CASE branch and checks function EXECUTE privileges in ExecInitFunc.
        // A runtime CASE cannot protect a forbidden callable; omit it from the statement altogether.
        // https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/executor/execExpr.c
        let requested = if self.requested { "pg_catalog.pg_is_wal_replay_paused()" } else { "NULL::boolean" };
        if version >= 140_000 {
            let state = if self.state { "pg_catalog.pg_get_wal_replay_pause_state()" } else { "NULL::text" };
            format!(
                "'replay_pause_requested',{requested},'replay_pause_state',{state},\
                 'replay_pause_access',pg_catalog.jsonb_build_object('requested',{},'state',{})",
                self.requested, self.state,
            )
        } else {
            format!(
                "'replay_paused',{requested},'replay_pause_state',NULL::text,\
                 'replay_pause_access',pg_catalog.jsonb_build_object('paused',{},'state',NULL::boolean)",
                self.requested,
            )
        }
    }
}

#[cfg(test)]
mod tests;
