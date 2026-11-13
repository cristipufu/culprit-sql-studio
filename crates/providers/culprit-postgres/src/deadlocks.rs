//! What blocked sessions wait for, and deadlocks. Postgres counts deadlocks per database but writes the details only to
//! the server log, which SQL can't read; Culprit shows the counter and explains pasted log lines.

use culprit_core::deadlock::classify;
use culprit_core::format::digits;
use culprit_core::telemetry::{
    MAX_POSTGRES_DEADLOCK_EDGES, postgres_deadlock_cycle_is_valid, postgres_log_lock_mode_is_valid,
};
use culprit_core::{
    Deadlock, DeadlockProcess, DeadlockReport, DeadlockResource, Engine, LockHolder, LockWait, Result, t,
};
use tokio_postgres::Client;

use crate::rows::{iso, query, query_one};

/// The lock each waiting backend asked for. A row-lock wait is a wait on another transaction's id; the table comes from
/// the tuple lock the waiter holds (or queues on), or else the table it is changing.
const LOCK_WAITS_SQL: &str = "
SELECT w.pid::text AS session, w.mode, w.locktype,
       coalesce(rel.name, tup.name, mod.name) AS object
  FROM pg_locks w
  LEFT JOIN LATERAL (SELECT n.nspname || '.' || c.relname AS name FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE c.oid = w.relation) rel ON true
  LEFT JOIN LATERAL (SELECT n.nspname || '.' || c.relname AS name FROM pg_locks t
                       JOIN pg_class c ON c.oid = t.relation JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE t.pid = w.pid AND t.locktype = 'tuple' LIMIT 1) tup ON true
  LEFT JOIN LATERAL (SELECT n.nspname || '.' || c.relname AS name FROM pg_locks t
                       JOIN pg_class c ON c.oid = t.relation JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE t.pid = w.pid AND t.locktype = 'relation' AND t.mode = 'RowExclusiveLock' AND c.relkind IN ('r', 'p')
                        AND n.nspname NOT IN ('pg_catalog', 'information_schema') LIMIT 1) mod ON true
 WHERE NOT w.granted AND w.pid <> pg_backend_pid()";

pub(crate) async fn lock_waits(client: &Client) -> Result<Vec<LockWait>> {
    let mut out: Vec<LockWait> = Vec::new();
    for r in query(client, LOCK_WAITS_SQL).await? {
        let session = r.string("session");
        if out.iter().any(|l| l.session == session) {
            continue;
        }
        out.push(LockWait {
            session,
            mode: r.text("mode"),
            resource: r.text("locktype"),
            object: r.text("object"),
            index: None,
        });
    }
    Ok(out)
}

pub(crate) async fn recent(client: &Client, since_hours: f64) -> Result<DeadlockReport> {
    let row = query_one(
        client,
        &format!(
            "SELECT deadlocks, {} AS since, current_setting('log_lock_waits') AS log_lock_waits
               FROM pg_stat_database WHERE datname = current_database()",
            iso("stats_reset")
        ),
    )
    .await?;
    let counted = row.as_ref().and_then(|r| r.number("deadlocks"));
    let since = row.as_ref().and_then(|r| r.text("since"));
    let log_waits = row.as_ref().and_then(|r| r.text("log_lock_waits")).is_some_and(|v| v == "on");
    let note = counted_note(counted, since.as_deref(), log_waits);
    Ok(DeadlockReport {
        deadlocks: Vec::new(),
        source: "pg_stat_database".into(),
        since_hours,
        counted,
        counted_since: since,
        note: Some(note),
        recent_only: false,
        lock_waits_logged: Some(log_waits),
    })
}

/// What the deadlock counter says, where the details are, and (with `log_lock_waits` off) how to log waits too.
fn counted_note(counted: Option<f64>, since: Option<&str>, log_waits: bool) -> String {
    let date = since.map(culprit_core::format::friendly_date_today);
    let count = |n: f64| digits(format!("{n:.0}"));
    let mut note = vec![
        match (counted, date) {
            (Some(n), None) => t!("provider.postgres.deadlocks.counted", n = n, count = count(n)),
            (Some(n), Some(date)) => {
                t!("provider.postgres.deadlocks.counted_since", n = n, count = count(n), date = date)
            }
            (None, None) => t!("provider.postgres.deadlocks.not_counted"),
            (None, Some(date)) => t!("provider.postgres.deadlocks.not_counted_since", date = date),
        },
        t!("provider.postgres.deadlocks.details"),
    ];
    if !log_waits {
        note.push(t!("provider.postgres.deadlocks.log_lock_waits"));
    }
    note.join(" ")
}

/// Deadlocks in pasted Postgres server log text (`ERROR: deadlock detected` with its DETAIL and CONTEXT lines).
pub fn parse_deadlock_log(text: &str) -> Vec<Deadlock> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (start, line) in lines.iter().enumerate() {
        if !line.contains("deadlock detected") {
            continue;
        }
        let block: Vec<&str> = lines[start + 1..]
            .iter()
            .take_while(|line| !line.contains("deadlock detected"))
            .take(40)
            .copied()
            .collect();
        let detail = block
            .iter()
            .take_while(|line| !pasted_section_start(line))
            .map(|line| line.split_once("DETAIL:").map_or(*line, |(_, detail)| detail))
            .collect::<Vec<_>>()
            .join("\n");
        let Some(mut deadlock) = detail_graph(&detail, log_time(line), "pasted log") else { continue };
        // Pasted text keeps its existing per-process statement presentation. Structured input instead supplies only
        // the reporting backend's separate native statement field: application SQL must not forge another process.
        for line in &block {
            attach_pasted_statement(&mut deadlock, line);
        }
        let reporter = bracketed_pid(line);
        let context = block.iter().find_map(|line| line.split_once("CONTEXT:").map(|(_, context)| context));
        qualify_deadlock(&mut deadlock, reporter.as_deref(), context);
        classify(Engine::Postgres, &mut deadlock);
        out.push(deadlock);
    }
    out
}

pub(crate) struct DeadlockLogContext<'a> {
    pub reporter: Option<&'a str>,
    pub statement: Option<&'a str>,
    pub context: Option<&'a str>,
    pub at: Option<String>,
}

/// SQLSTATE establishes a structured deadlock. The English DETAIL grammar can provide a graph, but a localized or
/// unsupported graph stays unavailable. No synthetic log header or application SQL is fed to this graph decoder.
pub(crate) fn parse_deadlock_details(detail: &str, context: DeadlockLogContext<'_>) -> Option<Deadlock> {
    let mut deadlock = detail_graph(detail, context.at, "postgres-structured-log")?;
    qualify_deadlock(&mut deadlock, context.reporter, context.context);
    if let (Some(statement), Some(victim)) =
        (context.statement, deadlock.processes.iter_mut().find(|process| process.victim))
    {
        victim.statement = Some(statement.to_owned());
    }
    classify(Engine::Postgres, &mut deadlock);
    Some(deadlock)
}

fn native_pid(value: &str) -> bool {
    value.parse::<i32>().is_ok_and(|pid| pid > 0)
}

/// PostgreSQL emits all wait edges before the explicit per-process query section. Arbitrary unsupported lines in
/// the graph cannot qualify a valid prefix; after the query boundary, multiline SQL is never parsed as another edge.
fn detail_graph(detail: &str, at: Option<String>, source: &str) -> Option<Deadlock> {
    let mut processes: Vec<DeadlockProcess> = Vec::new();
    let mut resources = Vec::new();
    for line in detail.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(pid) = detail_query_pid(line) {
            if !processes.iter().any(|process| process.id == pid) {
                return None;
            }
            break;
        }
        let (pid, mode, what, blocker) = detail_edge(line)?;
        if resources.len() == MAX_POSTGRES_DEADLOCK_EDGES {
            return None;
        }
        resources.push(DeadlockResource {
            id: what.into(),
            kind: what.split_whitespace().next()?.into(),
            object: None,
            index: None,
            holders: vec![LockHolder { process: blocker.into(), mode: None }],
            waiters: vec![LockHolder { process: pid.into(), mode: Some(mode.into()) }],
        });
        if !processes.iter().any(|process| process.id == pid) {
            processes.push(DeadlockProcess {
                id: pid.into(),
                session: Some(pid.into()),
                wait_mode: Some(mode.into()),
                ..Default::default()
            });
        }
    }
    if !postgres_deadlock_cycle_is_valid(resources.iter().flat_map(|resource| {
        resource
            .waiters
            .iter()
            .zip(&resource.holders)
            .map(|(waiter, holder)| (waiter.process.as_str(), holder.process.as_str()))
    })) {
        None
    } else {
        Some(Deadlock::new(source, at, processes, resources))
    }
}

fn detail_query_pid(line: &str) -> Option<&str> {
    let (pid, _) = line.strip_prefix("Process ")?.split_once(':')?;
    native_pid(pid).then_some(pid)
}

/// Pasted stderr records carry their other native sections outside DETAIL. Keep those headers out of the shared
/// native DETAIL decoder; an unknown line inside structured DETAIL must not be guessed into a section boundary.
fn pasted_section_start(line: &str) -> bool {
    let content = line.split_once("] ").map_or(line, |(_, content)| content).trim_start();
    ["HINT:", "CONTEXT:", "STATEMENT:", "ERROR:", "FATAL:", "PANIC:", "LOG:", "NOTICE:", "WARNING:", "INFO:"]
        .iter()
        .any(|prefix| content.starts_with(prefix))
}

fn detail_edge(line: &str) -> Option<(&str, &str, &str, &str)> {
    let (pid, rest) = line.strip_prefix("Process ")?.split_once(" waits for ")?;
    let (mode, rest) = rest.split_once(" on ")?;
    let (what, blocker) = rest.split_once("; blocked by process ")?;
    let blocker = blocker.strip_suffix('.')?;
    (native_pid(pid) && native_pid(blocker) && postgres_log_lock_mode_is_valid(mode) && !what.is_empty())
        .then_some((pid, mode, what, blocker))
}

fn attach_pasted_statement(deadlock: &mut Deadlock, line: &str) {
    let line = line.split_once("DETAIL:").map_or(line, |(_, detail)| detail).trim();
    if let Some((pid, statement)) = line.strip_prefix("Process ").and_then(|line| line.split_once(": "))
        && native_pid(pid)
        && let Some(process) = deadlock.processes.iter_mut().find(|process| process.id == pid)
    {
        process.statement = Some(statement.trim().into());
    }
}

fn qualify_deadlock(deadlock: &mut Deadlock, reporter: Option<&str>, context: Option<&str>) {
    for process in &mut deadlock.processes {
        process.victim = reporter == Some(process.id.as_str());
    }
    if let (Some(relation), Some(victim)) = (context.and_then(context_relation), reporter) {
        for resource in deadlock
            .resources
            .iter_mut()
            .filter(|resource| resource.waiters.iter().any(|waiter| waiter.process == victim))
        {
            resource.object = Some(relation.clone());
        }
    }
}

fn context_relation(context: &str) -> Option<String> {
    let context = context.trim();
    if !["while updating tuple ", "while deleting tuple ", "while locking tuple "]
        .iter()
        .any(|prefix| context.starts_with(prefix))
    {
        return None;
    }
    context.split_once(" in relation \"")?.1.strip_suffix('"').map(str::to_owned)
}

fn bracketed_pid(line: &str) -> Option<String> {
    let inside = line.split_once('[')?.1.split_once(']')?.0;
    inside.chars().all(|c| c.is_ascii_digit()).then(|| inside.to_string())
}

/// `2026-10-11 13:48:28.887 UTC [539] ERROR: …` → `2026-10-11T13:48:28Z` (only when the log says UTC).
fn log_time(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next()?);
    let looks_like_date = date.len() == 10 && date.chars().filter(|c| *c == '-').count() == 2;
    (looks_like_date && matches!(zone, "UTC" | "GMT"))
        .then(|| format!("{date}T{}Z", time.split('.').next().unwrap_or(time)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::DeadlockPattern;

    pub(crate) const LOG: &str = "2026-10-11 13:48:28.887 UTC [539] ERROR:  deadlock detected
2026-10-11 13:48:28.887 UTC [539] DETAIL:  Process 539 waits for ShareLock on transaction 838; blocked by process 545.
\tProcess 545 waits for ShareLock on transaction 837; blocked by process 539.
\tProcess 539: UPDATE customers SET id = id WHERE id = 1
\tProcess 545: UPDATE orders SET id = id WHERE id = 1
2026-10-11 13:48:28.887 UTC [539] HINT:  See server log for query details.
2026-10-11 13:48:28.887 UTC [539] CONTEXT:  while updating tuple (0,1) in relation \"customers\"
2026-10-11 13:48:28.887 UTC [539] STATEMENT:  UPDATE customers SET id = id WHERE id = 1";

    #[test]
    fn a_pasted_log_deadlock_is_explained() {
        let found = parse_deadlock_log(LOG);
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.at.as_deref(), Some("2026-10-11T13:48:28Z"));
        assert_eq!(d.processes.len(), 2);
        assert_eq!(d.victim().unwrap().id, "539");
        assert_eq!(d.processes[1].statement.as_deref(), Some("UPDATE orders SET id = id WHERE id = 1"));
        assert_eq!(d.pattern, DeadlockPattern::OppositeOrder);
        assert_eq!(d.objects, vec!["customers".to_string()]);
        assert!(d.fixes.iter().any(|f| f.detail.contains("40P01")));
        assert!(parse_deadlock_log("nothing here").is_empty());
    }

    // What the counter's note says, word for word.
    #[test]
    fn the_counter_note_in_english() {
        let details = "It writes the details (who waited for whom, and the statements) only to the server log, which \
                       SQL can't read: paste those log lines here to have them explained.";
        let log = "Turning on log_lock_waits also logs any lock wait longer than deadlock_timeout.";
        assert_eq!(
            counted_note(Some(3.0), None, true),
            format!("PostgreSQL counted 3 deadlocks in this database. {details}")
        );
        assert_eq!(
            counted_note(Some(1.0), Some("2024-10-01T00:00:00Z"), false),
            format!(
                "PostgreSQL counted 1 deadlock in this database since statistics were reset on Oct 1, 2024. {details} {log}"
            )
        );
        assert_eq!(
            counted_note(Some(0.0), Some("2024-10-01T00:00:00Z"), true),
            format!(
                "PostgreSQL counted 0 deadlocks in this database since statistics were reset on Oct 1, 2024. {details}"
            )
        );
        assert_eq!(
            counted_note(None, None, false),
            format!("PostgreSQL counted no deadlocks in this database. {details} {log}")
        );
        assert_eq!(
            counted_note(None, Some("2024-10-01T00:00:00Z"), true),
            format!(
                "PostgreSQL counted no deadlocks in this database since statistics were reset on Oct 1, 2024. {details}"
            )
        );
    }
}
