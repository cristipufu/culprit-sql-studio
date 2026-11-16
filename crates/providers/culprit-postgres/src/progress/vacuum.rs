//! VACUUM counters retain their native units and version-specific availability.
use super::selected;
use crate::rows::Row;
use culprit_core::{Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_vacuum";

pub(super) fn sql(database_oid: &str, actor: &ActivityKey, version: u32) -> Result<String> {
    // PostgreSQL 17 replaced tuple-capacity counters with dead-item byte/item counters.
    // https://www.postgresql.org/docs/17/progress-reporting.html#VACUUM-PROGRESS-REPORTING
    let dead_items = if version >= 170_000 {
        "NULL::text AS max_dead_tuples,NULL::text AS num_dead_tuples,\
         p.max_dead_tuple_bytes::text AS max_dead_tuple_bytes,p.dead_tuple_bytes::text AS dead_tuple_bytes,\
         p.num_dead_item_ids::text AS num_dead_item_ids,p.indexes_total::text AS indexes_total,\
         p.indexes_processed::text AS indexes_processed"
    } else {
        "p.max_dead_tuples::text AS max_dead_tuples,p.num_dead_tuples::text AS num_dead_tuples,\
         NULL::text AS max_dead_tuple_bytes,NULL::text AS dead_tuple_bytes,NULL::text AS num_dead_item_ids,\
         NULL::text AS indexes_total,NULL::text AS indexes_processed"
    };
    let delay = if version >= 180_000 { "p.delay_time::text" } else { "NULL::text" };
    selected::sql(
        SOURCE,
        &format!(
            "p.heap_blks_total::text AS heap_blks_total,p.heap_blks_scanned::text AS heap_blks_scanned,\
             p.heap_blks_vacuumed::text AS heap_blks_vacuumed,p.index_vacuum_count::text AS index_vacuum_count,\
             {dead_items},{delay} AS cost_delay_ms"
        ),
        database_oid,
        actor,
    )
}

pub(super) fn decode(row: &Row, database_oid: &str, actor: &ActivityKey) -> Result<MaintenanceProgressObservation> {
    selected::observation(
        row,
        database_oid,
        actor,
        MaintenanceCommand::Vacuum,
        None,
        MaintenanceProgressWork::PostgreSqlVacuum {
            progress: Box::new(PostgresVacuumProgress {
                heap_blks_total: row.unsigned_text("heap_blks_total")?,
                heap_blks_scanned: row.unsigned_text("heap_blks_scanned")?,
                heap_blks_vacuumed: row.unsigned_text("heap_blks_vacuumed")?,
                index_vacuum_count: row.unsigned_text("index_vacuum_count")?,
                max_dead_tuples: row.unsigned_text("max_dead_tuples")?,
                num_dead_tuples: row.unsigned_text("num_dead_tuples")?,
                max_dead_tuple_bytes: row.unsigned_text("max_dead_tuple_bytes")?,
                dead_tuple_bytes: row.unsigned_text("dead_tuple_bytes")?,
                num_dead_item_ids: row.unsigned_text("num_dead_item_ids")?,
                indexes_total: row.unsigned_text("indexes_total")?,
                indexes_processed: row.unsigned_text("indexes_processed")?,
                cost_delay_ms: selected::optional_text(row, "cost_delay_ms")?,
            }),
        },
    )
}
