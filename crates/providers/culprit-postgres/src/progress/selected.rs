//! A progress row is admitted only for the selected native backend and statement generation.
use crate::{native, rows::Row};
use culprit_core::{Result, telemetry::*};

pub(super) fn sql(view: &str, columns: &str, database_oid: &str, actor: &ActivityKey) -> Result<String> {
    let predicate = predicate(database_oid, actor)?;
    Ok(format!(
        "SELECT p.pid::text AS pid,p.datid::text AS database_oid,p.relid::text AS relation_oid,p.phase,\
         {columns},{backend_start} AS backend_start,{query_start} AS query_start,{at} AS observed_at \
         FROM pg_catalog.{view} p JOIN pg_catalog.pg_stat_activity a ON a.pid=p.pid AND a.datid=p.datid \
         WHERE {predicate} LIMIT 2",
        backend_start = native::iso("a.backend_start"),
        query_start = native::iso("a.query_start"),
        at = native::iso("pg_catalog.clock_timestamp()")
    ))
}

pub(super) fn predicate(database_oid: &str, actor: &ActivityKey) -> Result<String> {
    let pid = actor
        .session
        .id
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or_else(native::invalid)?;
    let database = database_oid.parse::<u32>().ok().filter(|oid| *oid > 0).ok_or_else(native::invalid)?;
    let backend = native::activity::generation_timestamp(&actor.session.generation)?;
    let request = actor.request.as_ref().ok_or_else(native::invalid)?;
    let query = native::activity::generation_timestamp(&request.generation)?;
    Ok(format!(
        "p.pid={pid} AND p.datid={database} AND a.backend_start='{backend}'::timestamptz \
         AND a.query_start='{query}'::timestamptz AND a.state='active' AND a.backend_type='client backend'"
    ))
}

pub(super) fn observation(
    row: &Row,
    database_oid: &str,
    expected: &ActivityKey,
    command: MaintenanceCommand,
    index_oid: Option<String>,
    work: MaintenanceProgressWork,
) -> Result<MaintenanceProgressObservation> {
    let pid = native::text(row, "pid")?;
    let backend = native::text(row, "backend_start")?;
    let query = native::text(row, "query_start")?;
    let actor = ActivityKey {
        session: NativeActivityIdentity { id: pid.clone(), generation: backend.clone() },
        request: Some(NativeActivityIdentity { id: "statement".into(), generation: query.clone() }),
        transaction: None,
    };
    if &actor != expected || native::text(row, "database_oid")? != database_oid {
        return Err(CollectionStop::SourceChanged.error());
    }
    let observation = MaintenanceProgressObservation {
        observed_at: canonical_time(&native::text(row, "observed_at")?)?,
        actor,
        statement: Some(native::activity::statement_identity(&pid, &backend, &query)),
        database_oid: database_oid.into(),
        relation_oid: native::text(row, "relation_oid")?,
        index_oid,
        command,
        phase: native::text(row, "phase")?,
        work,
    };
    if !observation.is_valid() {
        return Err(native::invalid());
    }
    Ok(observation)
}

/// An unsupported-version projection supplies an explicit SQL NULL. An absent alias is a malformed reply.
pub(super) fn optional_text(row: &Row, field: &str) -> Result<Option<String>> {
    row.value(field).map(|value| value.map(str::to_owned)).ok_or_else(native::invalid)
}
