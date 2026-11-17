//! The native command column distinguishes CLUSTER from VACUUM FULL.
use super::selected;
use crate::{native, rows::Row};
use culprit_core::{Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_cluster";

pub(super) fn sql(database_oid: &str, actor: &ActivityKey) -> Result<String> {
    selected::sql(
        SOURCE,
        "p.phase,p.command,p.cluster_index_relid::text AS index_oid,\
         p.heap_tuples_scanned::text AS heap_tuples_scanned,p.heap_tuples_written::text AS heap_tuples_written,\
         p.heap_blks_total::text AS heap_blks_total,p.heap_blks_scanned::text AS heap_blks_scanned,\
         p.index_rebuild_count::text AS index_rebuild_count",
        database_oid,
        actor,
    )
}

pub(super) fn decode(row: &Row, database_oid: &str, actor: &ActivityKey) -> Result<MaintenanceProgressObservation> {
    let command = match native::text(row, "command")?.as_str() {
        "CLUSTER" => MaintenanceCommand::Cluster,
        "VACUUM FULL" => MaintenanceCommand::VacuumFull,
        _ => return Err(native::invalid()),
    };
    selected::observation(
        row,
        database_oid,
        actor,
        command,
        Some(native::text(row, "index_oid")?),
        MaintenanceProgressWork::PostgreSqlCluster {
            progress: Box::new(PostgresClusterProgress {
                heap_tuples_scanned: row.unsigned_text("heap_tuples_scanned")?,
                heap_tuples_written: row.unsigned_text("heap_tuples_written")?,
                heap_blks_total: row.unsigned_text("heap_blks_total")?,
                heap_blks_scanned: row.unsigned_text("heap_blks_scanned")?,
                index_rebuild_count: row.unsigned_text("index_rebuild_count")?,
            }),
        },
    )
}
