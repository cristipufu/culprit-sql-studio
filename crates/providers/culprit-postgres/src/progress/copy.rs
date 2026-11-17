//! The selected COPY view is read once on the same actor/context owner as Live and retained work.
use super::{reader, selected};
use crate::{native, read::QueryRead, rows::Row};
use culprit_core::{Engine, ErrorCode, Result, telemetry::*};

pub(super) const SOURCE: &str = "pg_stat_progress_copy";
pub(super) const MINIMUM_VERSION: u32 = 140_000;

pub(super) struct Read {
    pub observation: Option<CurrentRequestWorkObservation>,
    pub unavailable: Option<ErrorCode>,
}

pub(super) async fn read<R: QueryRead>(
    reader: &mut R,
    database_oid: &str,
    database: &str,
    actor: &ActivityObservation,
    version: u32,
    sql: Option<&str>,
) -> Result<Read> {
    if version < MINIMUM_VERSION {
        return Ok(Read { observation: None, unavailable: Some(ErrorCode::Unsupported) });
    }
    let query = selected::sql(
        SOURCE,
        "p.command,p.type,p.bytes_processed::text AS bytes_processed,p.bytes_total::text AS bytes_total,\
         p.tuples_processed::text AS tuples_processed,p.tuples_excluded::text AS tuples_excluded,\
         pg_catalog.to_jsonb(p)->>'tuples_skipped' AS tuples_skipped",
        database_oid,
        &actor.key,
    )?;
    match reader.query(&query, 2).await {
        Ok(rows) if rows.len() > 1 => Err(native::invalid()),
        Ok(rows) => Ok(Read {
            observation: rows.first().map(|row| decode(row, database_oid, database, actor, sql)).transpose()?,
            unavailable: None,
        }),
        Err(error) if reader::optional(&error) => Ok(Read { observation: None, unavailable: Some(error.code) }),
        Err(error) => Err(error),
    }
}

fn decode(
    row: &Row,
    database_oid: &str,
    database: &str,
    actor: &ActivityObservation,
    sql: Option<&str>,
) -> Result<CurrentRequestWorkObservation> {
    let key = selected::actor(row, database_oid, &actor.key)?;
    if actor.database.as_deref() != Some(database) || actor.scope != ActivityScopeBasis::SessionDatabase {
        return Err(CollectionStop::SourceChanged.error());
    }
    let statement_identity = actor.statement_identity.clone().ok_or_else(native::invalid)?;
    let point = CurrentRequestWorkObservation {
        observed_at: canonical_time(&native::text(row, "observed_at")?)?,
        actor: key,
        database: database.into(),
        scope: actor.scope,
        statement_identity,
        statement: sql
            .map(|sql| DefinitionPayload::sql(sql, Engine::Postgres))
            .unwrap_or_else(|| actor.statement.clone()),
        native: CurrentRequestWorkNative::PostgresCopy {
            progress: Box::new(PostgresCopyProgress {
                database_oid: native::text(row, "database_oid")?,
                relation_oid: native::text(row, "relation_oid")?,
                command: PostgresCopyCommand::from_native(&native::text(row, "command")?)
                    .ok_or_else(native::invalid)?,
                io_type: PostgresCopyIo::from_native(&native::text(row, "type")?).ok_or_else(native::invalid)?,
                bytes_processed: row.unsigned_text("bytes_processed")?,
                bytes_total: row.unsigned_text("bytes_total")?,
                tuples_processed: row.unsigned_text("tuples_processed")?,
                tuples_excluded: row.unsigned_text("tuples_excluded")?,
                tuples_skipped: row.unsigned_text("tuples_skipped")?,
            }),
        },
    };
    if !point.is_valid() {
        return Err(native::invalid());
    }
    Ok(point)
}

#[cfg(test)]
mod tests;
