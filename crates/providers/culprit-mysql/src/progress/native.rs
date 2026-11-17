//! Exact current event facts; stage names never establish temporary storage, statement identity or causality.
use crate::{connect::Flavor, native, read::QueryRead, rows::Rec};
use culprit_core::{Engine, Result, telemetry::*};
use mysql_async::Value;

pub(super) struct StatementPoint {
    pub observation: CurrentRequestWorkObservation,
    pub state: String,
    pub sql: Option<String>,
}
pub(super) enum StatementRead {
    Observed(Box<StatementPoint>),
    Idle,
    Foreign,
    NotObserved,
    Unavailable,
}

pub(super) async fn statement<R: QueryRead>(
    reader: &mut R,
    session: u64,
    database: &str,
    instrumented: bool,
    sql_bytes: usize,
) -> Result<StatementRead> {
    let sql = native::activity::selected_request_query(
        "NULL AS info,
        CASE WHEN OCTET_LENGTH(e.SQL_TEXT) <= ? THEN e.SQL_TEXT ELSE NULL END AS sql_text,
        OCTET_LENGTH(e.SQL_TEXT) AS sql_text_bytes,
        e.ROWS_EXAMINED AS rows_examined, e.ROWS_SENT AS rows_sent,
        e.CREATED_TMP_TABLES AS created_tmp_tables, e.CREATED_TMP_DISK_TABLES AS created_tmp_disk_tables,
        e.SORT_MERGE_PASSES AS sort_merge_passes",
        2,
    );
    let rows = reader.read(&sql, vec![Value::UInt(sql_bytes as u64), Value::UInt(session), database.into()], 2).await?;
    match rows.as_slice() {
        [] => Ok(StatementRead::NotObserved),
        [row] => decode(row, database, instrumented, sql_bytes),
        _ => Err(native::invalid()),
    }
}

fn decode(row: &Rec, database: &str, instrumented: bool, sql_bytes: usize) -> Result<StatementRead> {
    let Some(actor) = native::activity::decode(row, database, instrumented)? else {
        return Ok(StatementRead::Unavailable);
    };
    if actor.database.as_deref() != Some(database) {
        return Ok(StatementRead::Foreign);
    }
    let Some(request) = &actor.key.request else {
        return Ok(if actor.idle { StatementRead::Idle } else { StatementRead::Unavailable });
    };
    // An explicit foreign request schema cannot inherit the session default database. Missing native request
    // schema remains the weaker SessionDatabase context supplied by the shared Activity decoder.
    let sql = row.native_text("sql_text")?;
    let source_bytes = row.unsigned("sql_text_bytes")?;
    if sql.as_ref().is_some_and(|text| text.len() > sql_bytes)
        || sql.as_ref().zip(source_bytes).is_some_and(|(text, bytes)| text.len() as u64 != bytes)
    {
        return Err(native::invalid());
    }
    let statement = match &sql {
        Some(sql) => DefinitionPayload::sql(sql, Engine::Mysql),
        None => DefinitionPayload::Unavailable {
            reason: if source_bytes.is_some_and(|bytes| bytes > sql_bytes as u64) {
                PayloadUnavailable::Oversized
            } else {
                PayloadUnavailable::NotCollected
            },
        },
    };
    let observation = CurrentRequestWorkObservation {
        observed_at: canonical_time(&native::text(row, "observed_at")?)?,
        actor: actor.key.clone(),
        database: database.into(),
        scope: actor.scope,
        statement_identity: actor.statement_identity.clone().ok_or_else(native::invalid)?,
        statement,
        native: CurrentRequestWorkNative::Mysql {
            work: Box::new(MysqlCurrentRequestWork {
                thread_id: actor.key.session.generation.clone(),
                statement_event_id: request.id.clone(),
                timer_start_ps: request.generation.clone(),
                timer_wait_ps: counter(row, "elapsed_ps")?,
                rows_examined: counter(row, "rows_examined")?,
                rows_sent: counter(row, "rows_sent")?,
                created_tmp_tables: counter(row, "created_tmp_tables")?,
                created_tmp_disk_tables: counter(row, "created_tmp_disk_tables")?,
                sort_merge_passes: counter(row, "sort_merge_passes")?,
                stage: MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::NotCollected },
            }),
        },
    };
    if !observation.is_valid() {
        return Err(native::invalid());
    }
    Ok(StatementRead::Observed(Box::new(StatementPoint { observation, state: actor.state, sql })))
}

pub(super) async fn stage<R: QueryRead>(
    reader: &mut R,
    request: &MysqlCurrentRequestWork,
    flavor: Flavor,
    version: &str,
) -> Result<MysqlStageObservation> {
    // MariaDB documents timers and parent events, but not MySQL's stage work columns. MySQL introduced these
    // columns in 5.7.5. Native availability does not authorize changing consumers or instruments.
    // https://dev.mysql.com/worklog/task/?id=7415
    // https://mariadb.com/docs/server/reference/system-tables/performance-schema/performance-schema-tables/performance-schema-events_stages_current-table
    let work = if flavor == Flavor::Mysql && culprit_core::version_at_least(version, [5, 7, 5]) {
        "WORK_COMPLETED AS work_completed, WORK_ESTIMATED AS work_estimated"
    } else {
        "NULL AS work_completed, NULL AS work_estimated"
    };
    let rows = reader
        .read(
            &format!(
                "SELECT THREAD_ID AS thread_id, EVENT_ID AS event_id, END_EVENT_ID AS end_event_id,
        NESTING_EVENT_ID AS nesting_event_id, NESTING_EVENT_TYPE AS nesting_event_type,
        EVENT_NAME AS event_name, TIMER_START AS timer_start_ps, TIMER_WAIT AS timer_wait_ps, {work}
        FROM performance_schema.events_stages_current WHERE THREAD_ID=? LIMIT 2"
            ),
            vec![Value::UInt(request.thread_id.parse().map_err(|_| native::invalid())?)],
            2,
        )
        .await?;
    match rows.as_slice() {
        [] => Ok(MysqlStageObservation::NotObserved),
        [row] => decode_stage(row, request),
        _ => Err(native::invalid()),
    }
}

fn decode_stage(row: &Rec, request: &MysqlCurrentRequestWork) -> Result<MysqlStageObservation> {
    if row.unsigned("end_event_id")?.is_some() {
        return Ok(MysqlStageObservation::NotObserved);
    }
    let thread_id = counter(row, "thread_id")?;
    let nesting_event_id = counter(row, "nesting_event_id")?;
    let nesting_event_type = row.native_text("nesting_event_type")?;
    if thread_id.as_ref() != Some(&request.thread_id)
        || nesting_event_id.as_ref() != Some(&request.statement_event_id)
        || nesting_event_type.as_deref() != Some("STATEMENT")
    {
        return Ok(MysqlStageObservation::Unlinked);
    }
    let event = MysqlStageEvent {
        thread_id: thread_id.ok_or_else(native::invalid)?,
        event_id: native::integer(row, "event_id")?.to_string(),
        nesting_event_id: nesting_event_id.ok_or_else(native::invalid)?,
        nesting_event_type: nesting_event_type.ok_or_else(native::invalid)?,
        event_name: native::text(row, "event_name")?,
        timer_start_ps: counter(row, "timer_start_ps")?,
        timer_wait_ps: counter(row, "timer_wait_ps")?,
        work_completed: counter(row, "work_completed")?,
        work_estimated: counter(row, "work_estimated")?,
    };
    let stage = MysqlStageObservation::Observed { event: Box::new(event) };
    let candidate = MysqlCurrentRequestWork { stage: stage.clone(), ..request.clone() };
    if !candidate.is_valid() {
        return Err(native::invalid());
    }
    Ok(stage)
}

fn counter(row: &Rec, field: &str) -> Result<Option<String>> {
    Ok(row.unsigned(field)?.map(|value| value.to_string()))
}

#[cfg(test)]
mod tests;
