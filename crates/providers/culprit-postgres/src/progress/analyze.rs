//! ANALYZE reports sampling and extended-statistics work, not a whole-command percentage.
use super::selected;
use crate::rows::Row;
use culprit_core::{Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_analyze";

pub(super) fn sql(database_oid: &str, actor: &ActivityKey, version: u32) -> Result<String> {
    let delay = if version >= 180_000 { "p.delay_time::text" } else { "NULL::text" };
    selected::sql(
        SOURCE,
        &format!(
            "p.phase,p.sample_blks_total::text AS sample_blks_total,p.sample_blks_scanned::text AS sample_blks_scanned,\
             p.ext_stats_total::text AS ext_stats_total,p.ext_stats_computed::text AS ext_stats_computed,\
             p.child_tables_total::text AS child_tables_total,p.child_tables_done::text AS child_tables_done,\
             p.current_child_table_relid::text AS current_child_table_relid,{delay} AS cost_delay_ms"
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
        MaintenanceCommand::Analyze,
        None,
        MaintenanceProgressWork::PostgreSqlAnalyze {
            progress: Box::new(PostgresAnalyzeProgress {
                sample_blks_total: row.unsigned_text("sample_blks_total")?,
                sample_blks_scanned: row.unsigned_text("sample_blks_scanned")?,
                ext_stats_total: row.unsigned_text("ext_stats_total")?,
                ext_stats_computed: row.unsigned_text("ext_stats_computed")?,
                child_tables_total: row.unsigned_text("child_tables_total")?,
                child_tables_done: row.unsigned_text("child_tables_done")?,
                current_child_table_relid: row.unsigned_text("current_child_table_relid")?,
                cost_delay_ms: selected::optional_text(row, "cost_delay_ms")?,
            }),
        },
    )
}
