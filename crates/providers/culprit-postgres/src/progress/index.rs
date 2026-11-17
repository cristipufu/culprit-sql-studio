//! The existing explicit index-progress query serves live display and retained capture with one decoder.
use super::selected;
use crate::{native, rows::Row};
use culprit_core::{Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_create_index";

pub(super) fn sql(database_oid: &str, actor: &ActivityKey) -> Result<String> {
    selected::sql(
        SOURCE,
        "p.index_relid::text AS index_oid,p.command,p.blocks_total::text AS blocks_total,p.blocks_done::text AS blocks_done,\
         p.tuples_total::text AS tuples_total,p.tuples_done::text AS tuples_done,\
         p.lockers_total::text AS lockers_total,p.lockers_done::text AS lockers_done,p.current_locker_pid::text AS current_locker_pid",
        database_oid,
        actor,
    )
}

pub(super) fn decode(row: &Row, database_oid: &str, expected: &ActivityKey) -> Result<MaintenanceProgressObservation> {
    let command = MaintenanceCommand::from_native(&native::text(row, "command")?).ok_or_else(native::invalid)?;
    if !matches!(
        command,
        MaintenanceCommand::CreateIndex
            | MaintenanceCommand::CreateIndexConcurrently
            | MaintenanceCommand::Reindex
            | MaintenanceCommand::ReindexConcurrently
    ) {
        return Err(native::invalid());
    }
    selected::observation(
        row,
        database_oid,
        expected,
        command,
        Some(native::text(row, "index_oid")?),
        MaintenanceProgressWork::PostgreSqlIndexBuild {
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
    )
}

#[cfg(test)]
mod tests;
