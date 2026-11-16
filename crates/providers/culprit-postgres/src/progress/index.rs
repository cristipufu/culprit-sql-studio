//! The existing explicit index-progress query serves live display and retained capture with one decoder.
use crate::{native, read::QueryRead, rows::Row};
use culprit_core::{Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_create_index";

pub(super) fn sql(database_oid: &str, actor: &ActivityKey) -> Result<String> {
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
        "SELECT p.pid::text AS pid,p.datid::text AS database_oid,p.relid::text AS relation_oid,p.index_relid::text AS index_oid,\
         p.command,p.phase,p.blocks_total::text AS blocks_total,p.blocks_done::text AS blocks_done,\
         p.tuples_total::text AS tuples_total,p.tuples_done::text AS tuples_done,\
         p.lockers_total::text AS lockers_total,p.lockers_done::text AS lockers_done,p.current_locker_pid::text AS current_locker_pid,\
         {backend_start} AS backend_start,{query_start} AS query_start,{at} AS observed_at \
         FROM pg_catalog.pg_stat_progress_create_index p JOIN pg_catalog.pg_stat_activity a ON a.pid=p.pid AND a.datid=p.datid \
         WHERE p.pid={pid} AND p.datid={database} AND a.backend_start='{backend}'::timestamptz \
         AND a.query_start='{query}'::timestamptz AND a.state='active' LIMIT 2",
        backend_start = native::iso("a.backend_start"),
        query_start = native::iso("a.query_start"),
        at = native::iso("pg_catalog.clock_timestamp()")
    ))
}

pub(super) async fn read<R: QueryRead>(
    reader: &mut R,
    database_oid: &str,
    actor: &ActivityKey,
) -> Result<Option<MaintenanceProgressObservation>> {
    let rows = reader.query(&sql(database_oid, actor)?, 2).await?;
    if rows.len() > 1 {
        return Err(native::invalid());
    }
    rows.first().map(|row| decode(row, database_oid, actor)).transpose()
}

pub(super) fn decode(row: &Row, database_oid: &str, expected: &ActivityKey) -> Result<MaintenanceProgressObservation> {
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
        index_oid: native::text(row, "index_oid")?,
        command: IndexBuildCommand::from_native(&native::text(row, "command")?).ok_or_else(native::invalid)?,
        phase: native::text(row, "phase")?,
        work: IndexBuildProgressWork::PostgreSql {
            progress: Box::new(PostgresIndexBuildProgress {
                blocks_total: row.unsigned_text("blocks_total")?,
                blocks_done: row.unsigned_text("blocks_done")?,
                tuples_total: row.unsigned_text("tuples_total")?,
                tuples_done: row.unsigned_text("tuples_done")?,
                lockers_total: row.unsigned_text("lockers_total")?,
                lockers_done: row.unsigned_text("lockers_done")?,
                current_locker_pid: row.unsigned_text("current_locker_pid")?,
            }),
        },
    };
    if !observation.is_valid() {
        return Err(native::invalid());
    }
    Ok(observation)
}
