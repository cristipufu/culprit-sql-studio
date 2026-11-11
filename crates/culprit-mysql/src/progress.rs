//! A running statement on another connection. MySQL: performance_schema's current statement and stage events (time so
//! far, rows examined, stage work done) and the plan it is executing (EXPLAIN FOR CONNECTION, which reads the running
//! statement's plan and never runs anything). MariaDB: the process list (time, rows examined, its own progress report)
//! and SHOW ANALYZE, which reads the running statement's plan with every operator's counters so far.

use culprit_core::{Engine, PlanKind, QueryProgress, Result, format, session_number, t};

use crate::connect::{Connector, Flavor, Scope};
use crate::rows::{explain_one, query_one};
use crate::{plan_json, plan_maria};

/// What MySQL can say about a running statement, where it says no more.
fn no_operator_progress() -> String {
    t!("provider.mysql.progress.no_operator_progress")
}

/// [`no_operator_progress`], when the plan it is executing couldn't be read either (the server's `error`).
fn plan_unavailable(error: &str) -> String {
    [no_operator_progress(), t!("provider.mysql.progress.plan_unavailable", error = error)].join(" ")
}

fn stage_progress_basis() -> String {
    t!("provider.mysql.progress.basis.stages")
}

fn mariadb_progress_basis() -> String {
    t!("provider.mysql.progress.basis.mariadb")
}

/// The phase a MariaDB statement that reports its progress is in: its `state` and its stage of `stages`.
fn mariadb_phase(state: Option<&str>, stage: f64, stages: f64) -> String {
    let staged = stages > 1.0;
    let (stage, stages) = (format::digits(format!("{stage:.0}")), format::digits(format!("{stages:.0}")));
    match (state, staged) {
        (Some(state), true) => t!("provider.mysql.progress.state_stage", state = state, stage = stage, stages = stages),
        (Some(state), false) => state.to_string(),
        (None, _) => t!("provider.mysql.progress.stage", stage = stage, stages = stages),
    }
}

/// TIMER_* columns are picoseconds; for a statement still running, TIMER_WAIT is the time so far.
const STATEMENT_SQL: &str = r#"
SELECT t.PROCESSLIST_STATE AS state, t.PROCESSLIST_COMMAND AS command, t.PROCESSLIST_DB AS db,
       s.SQL_TEXT AS sql_text, s.TIMER_WAIT / 1000000000 AS elapsed_ms, s.LOCK_TIME / 1000000000 AS lock_ms,
       s.ROWS_EXAMINED AS rows_examined, s.ROWS_SENT AS rows_sent, s.CREATED_TMP_DISK_TABLES AS tmp_disk,
       g.EVENT_NAME AS stage, g.WORK_COMPLETED AS work_completed, g.WORK_ESTIMATED AS work_estimated
  FROM performance_schema.threads t
  LEFT JOIN performance_schema.events_statements_current s ON s.THREAD_ID = t.THREAD_ID AND s.END_EVENT_ID IS NULL
  LEFT JOIN performance_schema.events_stages_current g ON g.THREAD_ID = t.THREAD_ID AND g.END_EVENT_ID IS NULL
 WHERE t.PROCESSLIST_ID = {id}
"#;

/// The same from the process list, for servers where performance_schema is off or hidden from this login.
const PROCESSLIST_SQL: &str = "SELECT STATE AS state, COMMAND AS command, DB AS db, INFO AS sql_text, TIME * 1000 AS elapsed_ms FROM information_schema.PROCESSLIST WHERE ID = {id}";

/// MariaDB's process list: milliseconds, rows examined so far, and progress for statements that report it.
const MARIADB_SQL: &str =
    "SELECT STATE AS state, COMMAND AS command, DB AS db, INFO AS sql_text, TIME_MS AS elapsed_ms,
       EXAMINED_ROWS AS rows_examined, PROGRESS AS progress, STAGE AS stage, MAX_STAGE AS max_stage
  FROM information_schema.PROCESSLIST WHERE ID = {id}";

/// What a MariaDB before 10.9 can say about a running statement.
fn mariadb_no_analyze() -> String {
    t!("provider.mysql.progress.no_show_analyze")
}

pub(crate) async fn query_progress(
    connector: &std::sync::Arc<Connector>,
    session: &str,
    with_plan: bool,
) -> Result<QueryProgress> {
    let id = session_number(session)?;
    let mut conn = crate::read::ReadSession::application(connector.clone(), Scope::Server);
    let taken_at = query_one(&mut conn, "SELECT DATE_FORMAT(UTC_TIMESTAMP(3), '%Y-%m-%dT%H:%i:%s.%fZ') AS now")
        .await?
        .map(|r| r.string("now"))
        .unwrap_or_default();
    let mut progress = QueryProgress::idle(session, Engine::Mysql, taken_at);
    if connector.flavor() == Flavor::Mariadb {
        let result = mariadb(&mut conn, id, with_plan, progress).await;
        conn.close().await;
        return result;
    }
    let detailed = query_one(&mut conn, &STATEMENT_SQL.replace("{id}", &id.to_string())).await;
    let row = match detailed {
        // Without the current-statement consumer the thread is there but its statement isn't.
        Ok(Some(row)) if row.text("command").is_some() && row.text("sql_text").is_some() => Some(row),
        Ok(Some(row)) if row.text("command").is_some_and(|c| c != "Query") => Some(row),
        _ => query_one(&mut conn, &PROCESSLIST_SQL.replace("{id}", &id.to_string())).await?,
    };
    let Some(row) = row else {
        conn.close().await;
        return Ok(progress);
    };
    let statement = row.text("sql_text").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    progress.running = statement.is_some() && row.text("command").as_deref() == Some("Query");
    progress.state = row.text("state").filter(|s| !s.is_empty()).or_else(|| row.text("command"));
    progress.statement = statement;
    if !progress.running {
        conn.close().await;
        return Ok(progress);
    }
    progress.elapsed_ms = row.number("elapsed_ms");
    // MySQL fills ROWS_EXAMINED in as the statement goes for some plans and only at the end for others: 0 means unknown.
    progress.rows_examined = row.number("rows_examined").filter(|r| *r > 0.0);
    // The thread state names a lock it is waiting for ("Waiting for table metadata lock"); LOCK_TIME is how long the
    // statement has waited for locks so far.
    if let Some(state) = progress.state.as_deref().filter(|s| s.to_ascii_lowercase().contains("lock")) {
        progress.wait = Some(state.to_string());
        progress.wait_ms = row.number("lock_ms").filter(|ms| *ms >= 1.0);
    }
    if let Some(stage) = row.text("stage") {
        let name = stage.trim_start_matches("stage/").replace('/', ": ");
        progress.phase = Some(name);
        if let (Some(done), Some(total)) = (row.number("work_completed"), row.number("work_estimated"))
            && total > 0.0
        {
            let fraction = (done / total).clamp(0.0, 1.0);
            progress.phase_percent = Some(fraction);
            progress.percent = Some(fraction);
            progress.percent_basis = Some(stage_progress_basis());
        }
    }
    if with_plan {
        // The session id is digits only (session_number), so splicing it in is safe.
        match explain_one(&mut conn, &format!("EXPLAIN FORMAT=JSON FOR CONNECTION {id}")).await {
            Ok(Some(rec)) => {
                if let Some(json) = rec.text("EXPLAIN").or_else(|| rec.text("explain"))
                    && let Ok(mut plan) = plan_json::parse(
                        &json,
                        PlanKind::Estimated,
                        progress.statement.as_deref(),
                        row.text("db").as_deref(),
                    )
                {
                    plan.raw = None;
                    progress.plan = Some(plan);
                }
            }
            Ok(None) => {}
            Err(e) => progress.note = Some(plan_unavailable(&e.message)),
        }
    }
    if progress.note.is_none() && progress.percent.is_none() {
        progress.note = Some(no_operator_progress());
    }
    conn.close().await;
    Ok(progress)
}

async fn mariadb(
    conn: &mut impl crate::read::QueryRead,
    id: u64,
    with_plan: bool,
    mut progress: QueryProgress,
) -> Result<QueryProgress> {
    let Some(row) = query_one(conn, &MARIADB_SQL.replace("{id}", &id.to_string())).await? else {
        return Ok(progress);
    };
    let statement = row.text("sql_text").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    progress.running = statement.is_some() && row.text("command").as_deref() == Some("Query");
    progress.state = row.text("state").filter(|s| !s.is_empty()).or_else(|| row.text("command"));
    progress.statement = statement;
    if !progress.running {
        return Ok(progress);
    }
    progress.elapsed_ms = row.number("elapsed_ms");
    progress.rows_examined = row.number("rows_examined").filter(|r| *r > 0.0);
    // ALTER TABLE, LOAD DATA, CHECK TABLE and friends report their own progress, per stage.
    if let Some(percent) = row.number("progress").filter(|p| *p > 0.0) {
        let fraction = (percent / 100.0).clamp(0.0, 1.0);
        let (stage, stages) = (row.number("stage").unwrap_or(1.0).max(1.0), row.number("max_stage").unwrap_or(1.0));
        progress.phase = Some(mariadb_phase(progress.state.as_deref(), stage, stages));
        progress.phase_percent = Some(fraction);
        progress.percent = Some((((stage - 1.0) + fraction) / stages.max(1.0)).clamp(0.0, 1.0));
        progress.percent_basis = Some(mariadb_progress_basis());
    }
    // The session id is digits only (session_number), so splicing it in is safe. SHOW ANALYZE never runs anything.
    match explain_one(conn, &format!("SHOW ANALYZE FORMAT=JSON FOR {id}")).await {
        Ok(Some(rec)) => {
            if let Some(json) = rec.first_text()
                && let Ok(mut plan) =
                    plan_json::parse(&json, PlanKind::Actual, progress.statement.as_deref(), row.text("db").as_deref())
            {
                plan.raw = None;
                progress.nodes = plan_maria::progress(&plan);
                progress.plan = Some(plan);
                if progress.percent.is_none() {
                    progress.summarize_nodes();
                } else {
                    let percent = progress.percent;
                    progress.summarize_nodes();
                    progress.percent = percent;
                    progress.percent_basis = Some(mariadb_progress_basis());
                }
                if !with_plan {
                    // Node ids come from the plan's shape, so they match the plan sent with the first reading.
                    progress.plan = None;
                }
            }
        }
        Ok(None) => {}
        // The statement finished between the two reads, or the server is older than 10.9.
        Err(e) if e.message.to_ascii_lowercase().contains("syntax") => progress.note = Some(mariadb_no_analyze()),
        Err(_) => {}
    }
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What a running statement's progress says, word for word: its messages come from locales/en.json.
    #[test]
    fn progress_says_what_it_can_in_english() {
        let mysql = "MySQL doesn't report how far a running query has got, operator by operator. Culprit shows the plan \
                     MySQL is executing, how long it has run and, for plans MySQL counts as it goes, how many rows it \
                     has examined so far. ALTER TABLE and other long statements report stage progress when \
                     performance_schema stage events are on.";
        assert_eq!(no_operator_progress(), mysql);
        assert_eq!(
            plan_unavailable("Unknown thread id: 42"),
            format!("{mysql} Its plan couldn't be shown: Unknown thread id: 42")
        );
        assert_eq!(
            mariadb_no_analyze(),
            "MariaDB reports a running statement's operators through SHOW ANALYZE, which needs MariaDB 10.9 or newer. \
             Culprit shows how long it has run and, for plans MariaDB counts as it goes, how many rows it has examined \
             so far."
        );
        assert_eq!(stage_progress_basis(), "MySQL's own stage progress (WORK_COMPLETED of WORK_ESTIMATED)");
        assert_eq!(mariadb_progress_basis(), "MariaDB's own progress report for this statement");
        assert_eq!(mariadb_phase(Some("altering table"), 2.0, 3.0), "altering table (stage 2 of 3)");
        assert_eq!(mariadb_phase(Some("copy to tmp table"), 1.0, 1.0), "copy to tmp table");
        assert_eq!(mariadb_phase(None, 1.0, 2.0), "stage 1 of 2");
    }
}
