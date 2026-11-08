use crate::{
    probe,
    sessions::{literal, target},
};
use culprit_core::{Engine, QueryProgress, Result, format, t};
use culprit_postgres::wire::{Connector, INTERACTIVE, ObservedSession, Row, SessionLimits, elevated_login, query_one};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Index,
    Analyze,
    Copy,
}

impl Command {
    fn for_statement(text: &str) -> Option<Self> {
        let statement = culprit_core::sql::check(text, Engine::Yugabyte);
        match statement.verb.as_str() {
            "CREATE" if culprit_core::sql::creates_index(text, Engine::Yugabyte) => Some(Self::Index),
            "REINDEX" => Some(Self::Index),
            "ANALYZE" => Some(Self::Analyze),
            "COPY" => Some(Self::Copy),
            _ => None,
        }
    }

    fn view(self) -> &'static str {
        match self {
            Self::Index => "pg_stat_progress_create_index",
            Self::Analyze => "pg_stat_progress_analyze",
            Self::Copy => "pg_stat_progress_copy",
        }
    }

    fn columns(self) -> &'static str {
        match self {
            Self::Index => "command,phase,tuples_done,tuples_total",
            Self::Analyze => "phase",
            Self::Copy => "command,type,yb_status,tuples_processed,bytes_processed,bytes_total",
        }
    }

    fn describe(self, row: &Row) -> Option<(String, String)> {
        let count = |key| row.number(key).filter(|n| *n >= 0.0 && n.fract() == 0.0);
        match self {
            Self::Index => Some(index_step(
                row.text("command")?,
                row.text("phase")?,
                count("tuples_done"),
                count("tuples_total").filter(|n| *n > 0.0),
            )),
            Self::Analyze => Some(analyze_step(row.text("phase")?)),
            Self::Copy => {
                // YugabyteDB retains the last COPY result on this backend after SUCCESS or ERROR.
                if row.text("yb_status").as_deref() != Some("IN PROGRESS") {
                    return None;
                }
                Some(copy_step(
                    row.text("command")?,
                    row.text("type").unwrap_or_default(),
                    count("tuples_processed"),
                    count("bytes_processed"),
                    count("bytes_total").filter(|n| *n > 0.0),
                ))
            }
        }
    }
}

/// An index build's phase, with the table rows its backfill has processed and expects, and what those counts are.
fn index_step(command: String, phase: String, done: Option<f64>, total: Option<f64>) -> (String, String) {
    let mut text = format!("{command}: {phase}");
    if phase == "backfilling" {
        if let Some(done) = done {
            let processed = t!("provider.yugabyte.progress.rows_processed", n = done, count = format::count(done));
            text.push_str(&format!(" · {processed}"));
        }
        if let Some(total) = total {
            let estimated = t!("provider.yugabyte.progress.rows_estimated", n = total, count = format::count(total));
            text.push_str(&format!(" · {estimated}"));
        }
    }
    (text, t!("provider.yugabyte.progress.index_note"))
}

/// ANALYZE's phase, and what it doesn't measure.
fn analyze_step(phase: String) -> (String, String) {
    (format!("ANALYZE: {phase}"), t!("provider.yugabyte.progress.analyze_note"))
}

/// A COPY in progress: the rows and bytes it reports processed, its source file's size, and what those counts are.
fn copy_step(
    command: String,
    kind: String,
    rows: Option<f64>,
    bytes: Option<f64>,
    total: Option<f64>,
) -> (String, String) {
    let mut text = format!("{command} {kind}: IN PROGRESS");
    if let Some(rows) = rows {
        let processed = t!("provider.yugabyte.progress.copy_rows", n = rows, count = format::count(rows));
        text.push_str(&format!(" · {processed}"));
    }
    if let Some(bytes) = bytes {
        let processed = t!("provider.yugabyte.progress.copy_bytes", bytes = format::bytes(bytes));
        text.push_str(&format!(" · {processed}"));
    }
    if let Some(total) = total {
        let file = t!("provider.yugabyte.progress.copy_file", bytes = format::bytes(total));
        text.push_str(&format!(" · {file}"));
    }
    (text, t!("provider.yugabyte.progress.copy_note"))
}

pub(crate) async fn read(connector: &Connector, id: &str, with_plan: bool) -> Result<(QueryProgress, bool)> {
    target(id)?;
    let session = connector.connect_observed(SessionLimits { catalog_path: true, ..INTERACTIVE }).await?;
    read_session(&session, id, with_plan).await
}

pub(crate) async fn read_session(
    session: &ObservedSession,
    id: &str,
    with_plan: bool,
) -> Result<(QueryProgress, bool)> {
    let (node, pid, started) = target(id)?;
    let p = probe::read(&session.client).await?;
    let relation = if p.global { "pg_catalog.\"gv$pg_stat_activity\"" } else { "pg_catalog.pg_stat_activity" };
    let node_guard = if p.global {
        // Keep the native UUID column bare so the global FDW prunes unrelated nodes during planning.
        format!("a.server_uuid={}::uuid", literal(&node))
    } else {
        format!("replace(yb_get_local_tserver_uuid()::text,'-','')={}", literal(&node))
    };
    let notices = session.notices();
    let activity = query_one(
        &session.client,
        &format!(
            "SELECT a.state,a.query,a.backend_type,a.wait_event_type,a.wait_event,{} AS query_start,
        extract(epoch FROM clock_timestamp()-a.query_start)*1000 AS elapsed_ms,
        (pg_get_userbyid(a.usesysid)=session_user AND current_user=session_user) AS own
        FROM {relation} a WHERE a.datid={} AND a.pid={pid} AND a.backend_start={}::timestamptz AND {node_guard}",
            probe::timestamp("a.query_start"),
            p.oid,
            literal(&started)
        ),
    )
    .await?;
    let mut progress = QueryProgress::idle(id, Engine::Yugabyte, p.now);
    progress.note = Some(t!("provider.yugabyte.progress.note"));
    let Some(row) = activity else {
        progress.note = Some(t!("provider.yugabyte.progress.backend_gone"));
        return Ok((progress, false));
    };
    progress.state = row.text("state");
    progress.running = row.text("state").as_deref() == Some("active");
    progress.statement = row.text("query").filter(|s| s != "<insufficient privilege>");
    progress.elapsed_ms = row.number("elapsed_ms").filter(|n| *n >= 0.0 && progress.running);
    progress.wait = match (row.text("wait_event_type"), row.text("wait_event")) {
        (Some(kind), Some(event)) => Some(format!("{kind}: {event}")),
        (kind, event) => event.or(kind),
    };
    let worker = row.text("backend_type").as_deref() == Some("yb-conn-mgr worker connection");
    let partial = session.notices() != notices;
    if worker {
        progress.note = Some(t!("provider.yugabyte.progress.pooled_worker"));
    } else if partial || progress.state.is_none() {
        progress.note = Some(t!("provider.yugabyte.progress.partial"));
    }
    // Native YSQL progress views can also return retained COPY counters for an unrelated command. Select the view
    // with the shared SQL lexer (including comments), then require its own native command/status metadata.
    // Pin query_start as well as backend_start: one backend can start a different command between these reads.
    if progress.running
        && !worker
        && !partial
        && let (Some(query_start), Some(command)) =
            (row.text("query_start"), progress.statement.as_deref().and_then(Command::for_statement))
    {
        let view = command.view();
        let command_relation =
            if p.global { format!("pg_catalog.\"gv${view}\"") } else { format!("pg_catalog.{view}") };
        let guard = if p.global { format!(" AND server_uuid={}::uuid", literal(&node)) } else { String::new() };
        let before = session.notices();
        let phase = query_one(
            &session.client,
            &format!(
                "SELECT {} FROM {command_relation} WHERE pid={pid}{guard} AND datid={} AND EXISTS(
                    SELECT 1 FROM {relation} a WHERE a.datid={} AND a.pid={pid}
                    AND a.backend_start={}::timestamptz AND {node_guard}
                    AND a.query_start={}::timestamptz AND a.state='active') LIMIT 1",
                command.columns(),
                p.oid,
                p.oid,
                literal(&started),
                literal(&query_start)
            ),
        )
        .await;
        match phase {
            Ok(Some(row)) if session.notices() == before => {
                if let Some((text, note)) = command.describe(&row) {
                    progress.phase = Some(text);
                    progress.note = Some(format!("{} {note}", progress.note.as_deref().unwrap_or_default()));
                }
            }
            Ok(None) if session.notices() == before => {}
            _ => {
                progress.note = Some(t!("provider.yugabyte.progress.view_unavailable"));
            }
        }
    }
    let allowed = with_plan
        && progress.running
        && !worker
        && !partial
        && progress.statement.is_some()
        && (row.flag("own") || !elevated_login(&session.client).await?);
    Ok((progress, allowed))
}

#[cfg(test)]
mod tests {
    use super::{Command, analyze_step, copy_step, index_step};

    // What a command's progress says, word for word.
    #[test]
    fn progress_steps_in_english() {
        assert_eq!(
            index_step("CREATE INDEX".into(), "backfilling".into(), Some(1500.0), Some(2_400_000.0)),
            (
                "CREATE INDEX: backfilling · 1.5K table rows processed · about 2.4M table rows estimated".to_string(),
                "Index backfill counts describe table rows, including for partial indexes. The total comes from an \
                 estimate; it is not a measured completion percentage."
                    .to_string()
            )
        );
        assert_eq!(
            index_step("CREATE INDEX".into(), "initializing".into(), Some(5.0), None).0,
            "CREATE INDEX: initializing"
        );
        assert_eq!(
            analyze_step("acquiring sample rows".into()),
            (
                "ANALYZE: acquiring sample rows".to_string(),
                "ANALYZE reports its current native phase. PostgreSQL heap-block counters do not measure DocDB progress."
                    .to_string()
            )
        );
        assert_eq!(
            copy_step("COPY FROM".into(), "FILE".into(), Some(20.0), Some(2048.0), Some(1_048_576.0)),
            (
                "COPY FROM FILE: IN PROGRESS · 20 rows reported processed · 2.0 KB processed · 1.0 MB source file"
                    .to_string(),
                "Native row counters follow the source's transaction mode. Batched COPY FROM can leave completed \
                 transactions persisted after a later error. A zero or missing file size is unknown; counters do not \
                 prove whole-command completion or commit."
                    .to_string()
            )
        );
        assert_eq!(copy_step("COPY FROM".into(), String::new(), None, None, None).0, "COPY FROM : IN PROGRESS");
    }

    #[test]
    fn native_progress_requires_an_unambiguous_current_command() {
        assert_eq!(Command::for_statement("/* CREATE INDEX fake */ COPY t FROM STDIN"), Some(Command::Copy));
        assert_eq!(
            Command::for_statement("-- application\nCREATE /* nested /* comment */ */ UNIQUE INDEX i ON t(c)"),
            Some(Command::Index)
        );
        assert_eq!(Command::for_statement("REINDEX INDEX i"), Some(Command::Index));
        assert_eq!(Command::for_statement("ANALYZE t"), Some(Command::Analyze));
        for text in [
            "SELECT 'COPY t FROM STDIN'",
            "CREATE TABLE t (copy text)",
            "COPY t FROM STDIN; SELECT 1",
            "COPY t FROM 'truncated",
        ] {
            assert_eq!(Command::for_statement(text), None, "{text}");
        }
    }
}
