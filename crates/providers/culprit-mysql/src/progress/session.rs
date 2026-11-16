//! An interactive process-list point has a session default database, never a native request generation.
use super::*;
use crate::rows::Rec;
use culprit_core::{SessionProgressContext, t};
use mysql_async::Value;

pub(super) struct Capture {
    point: Option<Point>,
}

struct Point {
    session: String,
    at: String,
    database: String,
    command: String,
    state: Option<String>,
    sql: Option<String>,
    stage: Option<(u64, u64, Option<f64>)>,
}

pub(super) async fn read<R: QueryRead>(
    reader: &mut R,
    session: u64,
    context: &Context,
    sql_bytes: usize,
) -> Result<Capture> {
    // These are existing interactive native facts, not a retained request-work or plan source. MySQL TIME is
    // time in the current state; it cannot populate the statement elapsed-time field.
    // https://dev.mysql.com/doc/refman/8.4/en/information-schema-processlist-table.html
    // https://mariadb.com/docs/server/reference/system-tables/information-schema/information-schema-tables/information-schema-processlist-table
    let stage = match context.connection.flavor {
        Flavor::Mysql => "NULL AS stage,NULL AS max_stage,NULL AS progress",
        Flavor::Mariadb => "STAGE AS stage,MAX_STAGE AS max_stage,PROGRESS AS progress",
    };
    let sql = format!(
        "SELECT ID AS id,DB AS db,COMMAND AS command,STATE AS state,
        CASE WHEN OCTET_LENGTH(INFO)<=? THEN INFO ELSE NULL END AS sql_text,
        OCTET_LENGTH(INFO) AS sql_text_bytes,{stage},
        DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at
        FROM information_schema.PROCESSLIST WHERE ID=? AND DB=? AND ID<>CONNECTION_ID()
        AND COMMAND NOT IN ('Daemon','Binlog Dump','Binlog Dump GTID') AND USER<>'system user'
        AND (INFO IS NULL OR (INFO NOT LIKE '%/* culprit */%' AND INFO NOT LIKE '%/* culprit:%')) LIMIT 2"
    );
    let rows = match reader
        .read(
            &sql,
            vec![Value::UInt(sql_bytes as u64), session.into(), context.connection.database_name.clone().into()],
            2,
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) if source_native::local_context::unavailable(&error) => return Ok(Capture { point: None }),
        Err(error) => return Err(error),
    };
    let point = match rows.as_slice() {
        [] => None,
        [row] => decode(row, session, context, sql_bytes)?,
        _ => return Err(source_native::invalid()),
    };
    Ok(Capture { point })
}

fn decode(row: &Rec, session: u64, context: &Context, sql_bytes: usize) -> Result<Option<Point>> {
    if row.unsigned("id")? != Some(session) || session == context.connection.connection_id {
        return Err(source_native::invalid());
    }
    let Some(database) = row.native_text("db")?.filter(|database| database == &context.connection.database_name) else {
        return Ok(None);
    };
    let command = source_native::text(row, "command")?;
    if matches!(command.as_str(), "Daemon" | "Binlog Dump" | "Binlog Dump GTID") {
        return Ok(None);
    }
    let sql = row.native_text("sql_text")?;
    let source_bytes = row.unsigned("sql_text_bytes")?;
    if sql
        .as_ref()
        .is_some_and(|sql| sql.len() > sql_bytes || sql.contains(crate::connect::TAG) || sql.contains("/* culprit:"))
        || sql.as_ref().zip(source_bytes).is_some_and(|(sql, bytes)| sql.len() as u64 != bytes)
    {
        return Err(source_native::invalid());
    }
    let index = row.unsigned("stage")?;
    let total = row.unsigned("max_stage")?;
    if !row.has_column("progress") {
        return Err(source_native::invalid());
    }
    let percent = row
        .text("progress")
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                .ok_or_else(source_native::invalid)
        })
        .transpose()?;
    let stage = index
        .zip(total)
        .filter(|(index, total)| *index > 0 && index <= total)
        .map(|(index, total)| (index, total, percent.map(|percent| percent / 100.0)));
    Ok(Some(Point {
        session: session.to_string(),
        at: canonical_time(&source_native::text(row, "observed_at")?)?,
        database,
        command,
        state: row.native_text("state")?.filter(|state| !state.is_empty()),
        sql,
        stage,
    }))
}

impl Capture {
    pub(super) fn validate(&self, context: &NativeReadContext) -> Result<()> {
        if self.point.as_ref().is_some_and(|point| !point.context(context.clone()).is_valid()) {
            return Err(source_native::invalid());
        }
        Ok(())
    }

    pub(super) fn project(self, session: &str, context: NativeReadContext) -> QueryProgress {
        let mut progress = QueryProgress::unobserved(session, Engine::Mysql, context.observed_until.clone());
        progress.running = None;
        let Some(point) = self.point else {
            progress.note = Some(t!("provider.mysql.progress.unavailable"));
            return progress;
        };
        progress.note = Some(t!("provider.mysql.progress.independent_session"));
        let session_context = point.context(context);
        progress.running = match point.command.as_str() {
            "Sleep" => Some(false),
            "Query" | "Execute" => Some(true),
            _ => None,
        };
        progress.taken_at = point.at.clone();
        progress.statement = point.sql;
        progress.state = point.state.clone().or(Some(point.command));
        if let Some((index, total, percent)) = point.stage {
            progress.phase = Some(super::live::mariadb_phase(point.state.as_deref(), index, total));
            progress.phase_percent = percent;
        }
        progress.session_context = Some(Box::new(session_context));
        progress
    }
}

impl Point {
    fn context(&self, context: NativeReadContext) -> SessionProgressContext {
        SessionProgressContext {
            source: "information_schema.PROCESSLIST".into(),
            context,
            observed_at: self.at.clone(),
            session: self.session.clone(),
            database: self.database.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
