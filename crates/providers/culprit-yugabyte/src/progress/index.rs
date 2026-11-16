//! YSQL index progress is local to the issuing backend. Heap blocks and PostgreSQL lockers are not YSQL work.
use crate::{native, probe, read::QueryRead, sessions::literal};
use culprit_core::{Result, telemetry::*};
use culprit_postgres::wire::{Row, activity};

pub(super) const SOURCE: &str = "pg_stat_progress_create_index";

pub(super) fn sql(database: u32, node: &str, actor: &ActivityKey) -> Result<String> {
    let (actor_node, pid) = actor.session.id.split_once('/').ok_or_else(native::invalid)?;
    if actor_node != node || probe::node_id(node).as_deref() != Some(node) || database == 0 {
        return Err(native::invalid());
    }
    let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 0).ok_or_else(native::invalid)?;
    let backend = activity::generation_timestamp(&actor.session.generation)?;
    let request = activity::generation_timestamp(&actor.request.as_ref().ok_or_else(native::invalid)?.generation)?;
    Ok(format!(
        "SELECT p.pid::text AS pid,p.datid::text AS database_oid,p.relid::text AS relation_oid,p.index_relid::text AS index_oid,\
         p.command,p.phase,p.tuples_done::text AS tuples_done,p.tuples_total::text AS tuples_total,\
         {backend_start} AS backend_start,{query_start} AS query_start,{at} AS observed_at \
         FROM pg_catalog.pg_stat_progress_create_index p JOIN pg_catalog.pg_stat_activity a ON a.pid=p.pid AND a.datid=p.datid \
         WHERE p.pid={pid} AND p.datid={database} AND a.backend_start={backend}::timestamptz \
         AND a.query_start={request}::timestamptz AND a.state='active' \
         AND replace(yb_get_local_tserver_uuid()::text,'-','')={node} LIMIT 2",
        backend_start = probe::timestamp("a.backend_start"),
        query_start = probe::timestamp("a.query_start"),
        at = probe::timestamp("clock_timestamp()"),
        backend = literal(&backend),
        request = literal(&request),
        node = literal(node),
    ))
}

pub(super) async fn read(
    reader: &mut impl QueryRead,
    database: u32,
    node: &str,
    actor: &ActivityKey,
) -> Result<Option<MaintenanceProgressObservation>> {
    let rows = reader.query(&sql(database, node, actor)?, 2).await?;
    if rows.len() > 1 {
        return Err(native::invalid());
    }
    rows.first().map(|row| decode(row, database, node, actor)).transpose()
}

fn decode(row: &Row, database: u32, node: &str, expected: &ActivityKey) -> Result<MaintenanceProgressObservation> {
    let pid = native::text(row, "pid")?;
    let backend = native::text(row, "backend_start")?;
    let request = native::text(row, "query_start")?;
    let actor = ActivityKey {
        session: NativeActivityIdentity { id: format!("{node}/{pid}"), generation: backend.clone() },
        request: Some(NativeActivityIdentity { id: "statement".into(), generation: request.clone() }),
        transaction: None,
    };
    if &actor != expected || native::text(row, "database_oid")? != database.to_string() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let point = MaintenanceProgressObservation {
        observed_at: canonical_time(&native::text(row, "observed_at")?)?,
        actor,
        statement: Some(activity::statement_identity(&pid, &backend, &request).qualified("node", node)),
        database_oid: database.to_string(),
        relation_oid: native::text(row, "relation_oid")?,
        index_oid: Some(native::text(row, "index_oid")?),
        command: MaintenanceCommand::from_native(&native::text(row, "command")?).ok_or_else(native::invalid)?,
        phase: native::text(row, "phase")?,
        work: MaintenanceProgressWork::YsqlIndexBuild { progress: Box::new(work(row)?) },
    };
    if !point.is_valid() {
        return Err(native::invalid());
    }
    Ok(point)
}

pub(super) fn work(row: &Row) -> Result<YsqlIndexBuildProgress> {
    Ok(YsqlIndexBuildProgress {
        tuples_done: row.unsigned_text("tuples_done")?,
        tuples_total_estimate: row.unsigned_text("tuples_total")?,
    })
}
