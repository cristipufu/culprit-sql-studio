//! Native replication state and measurements; connection strings and credentials are never selected.
use super::*;
use culprit_core::{MeasurementScope, Replica, ReplicaCondition, ReplicaRole, telemetry::*};
use serde_json::Value;
use std::collections::BTreeMap;
mod decode;
mod pause;
mod project;
mod scan;
mod sql;
pub(crate) use decode::decode;
pub(crate) use pause::ReplayPauseAccess;
pub(crate) use project::project;
pub(crate) use scan::acquire;
pub(crate) use sql::query;
#[cfg(test)]
mod tests;

pub(crate) fn sources(standby: bool) -> Vec<String> {
    if standby {
        vec!["pg_recovery".into(), "pg_replication_slots".into()]
    } else {
        vec!["pg_stat_replication".into(), "pg_replication_slots".into()]
    }
}
