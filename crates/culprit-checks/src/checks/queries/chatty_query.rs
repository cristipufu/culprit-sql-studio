//! The `chatty-query` check: Tiny queries executed so often that the round trips cost more than the work, typically a loop in application code.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::{ScanSpan, key, t};

pub fn chatty_query_info() -> CheckInfo {
    CheckInfo {
        id: "chatty-query".into(),
        title: t!("check.chatty_query.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![],
        description: t!("check.chatty_query.info.description"),
    }
}

check!(
    ChattyQueryCheck: chatty_query_info,
    decide = chatty_queries,
    render = chatty_query_finding,
    case<'s> = ChattyQuery<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Work)],
);

/// Round trips a day for a chatty statement to be Medium: at about a millisecond each, the minute a day
/// ([`MEDIUM_MS_A_DAY`]) Medium asks of database time.
pub(super) const MEDIUM_ROUND_TRIPS_A_DAY: f64 = MEDIUM_MS_A_DAY;

/// A tiny statement sent so often that its round trips are the cost, and what its counters establish.
pub struct ChattyQuery<'s> {
    query: &'s QueryStat,
    impact: QueryImpact,
    /// Its share of the statements' runs, over Culprit's scans where they measured it.
    exec_share: f64,
    /// Its runs a day, where they came that way.
    per_day_runs: Option<f64>,
    /// Its runs came in bursts: said by when they came, never a rate a day.
    burst: bool,
    /// It ran in a burst or stopped running: told in the past.
    past: bool,
    /// Rows a run changed (a write) or returned (a read).
    rows: f64,
    shape: Shape,
    /// What Culprit's scans measured of it, where they did (not a burst's).
    scans: Option<ScanSpan>,
    /// The rows a run handles are kept only in the source's totals, which span more than its scans.
    rows_counted_over: bool,
    severity: Severity,
    score: f64,
}

/// Shapes that look alike in the counters and need different fixes.
enum Shape {
    /// Most runs find nothing: a poll, a check, a miss.
    FindsNothing { writes: bool },
    /// A single-row write: a counter handed out one number at a time, or an update, delete or insert repeated row by
    /// row (or a heartbeat).
    Write(WriteKind),
    /// A read of one key a call (N+1).
    Read,
}

fn chatty_queries(snapshot: &Snapshot) -> Vec<ChattyQuery<'_>> {
    let Some(workload) = &snapshot.workload else {
        return Vec::new();
    };
    if workload.total_executions <= 0.0 {
        return Vec::new();
    }
    workload
        .queries
        .iter()
        // BEGIN and COMMIT around every transaction, a pool's SELECT 1, a SET per checkout: frequent and tiny by
        // nature, and no loop in the application sends them.
        .filter(|q| !is_monitoring_query(q) && !culprit_core::sql::is_session_statement(&q.text))
        .filter(|q| {
            // Its share of the runs and its average as every surface states them: over Culprit's scans where they
            // measured it.
            let exec_share = workload.statement_runs_share(q);
            // Busy by the rate it ran at while it ran: a burst by its own total, never "a day" of six seconds.
            let busy = at_least_per_day_of(snapshot, q, q.executions, 20_000.0);
            let small = q.rows_handled_per_execution().is_some_and(|r| r <= 2.0) && workload.statement_mean_ms(q) < 5.0;
            small && exec_share >= 0.08 && busy
        })
        .take(4)
        .map(|q| {
            let exec_share = workload.statement_runs_share(q);
            let burst = ran_in_bursts(snapshot, q);
            let impact = query_impact(snapshot, &[q]).expect("workload present");
            // Rows changed for a write, rows returned otherwise: MySQL counts a write's rows apart, and returns none.
            let rows = q.rows_handled_per_execution().unwrap_or(1.0);
            let shape = if rows < 0.5 {
                Shape::FindsNothing { writes: q.writes_rows() }
            } else if q.writes_rows() {
                Shape::Write(WriteKind::of(&q.text, snapshot.server.engine))
            } else {
                Shape::Read
            };
            // Its runs and their share as Culprit's scans measured them, where they did (not a burst's, said by when it
            // ran); the rows a run handles are kept only in the totals, named so beside them.
            let scans = workload.scans_of(q).filter(|_| !burst);
            // A share of the executions is easy to reach on a quiet database; the amount is the round trips: each costs
            // the application about a millisecond in the same region, so Medium asks for the minute a day it asks of
            // database time; a burst's are its own total, never extrapolated to a day.
            let medium = exec_share >= 0.25 && at_least_per_day_of(snapshot, q, q.executions, MEDIUM_ROUND_TRIPS_A_DAY);
            ChattyQuery {
                query: q,
                exec_share,
                per_day_runs: workload.runs_per_day_of(q),
                burst,
                // A burst, or a statement that stopped running, is told in the past.
                past: burst || crate::all_stopped(snapshot, &[q]),
                rows,
                shape,
                scans,
                rows_counted_over: scans.is_some() && crate::support::totals_basis(snapshot, q).is_some(),
                severity: if medium { Severity::Medium } else { Severity::Low },
                score: (score_for_share(impact.share) + exec_share * 40.0).min(60.0),
                impact,
            }
        })
        .collect()
}

fn chatty_query_finding(snapshot: &Snapshot, chatty: ChattyQuery<'_>) -> Finding {
    let q = chatty.query;
    let workload = snapshot.workload.as_ref().expect("the check needs a workload");
    let mut f = finding("chatty-query", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let query = capitalize(&query_handle(q));
    // As its runs came: a burst by when it ran ("ran 30K times on Oct 17 at 10:17 UTC").
    let runs = runs_of(snapshot, q);
    let tense = if chatty.past { "past" } else { "present" };
    let share =
        t!("check.chatty_query.share", share = format::percent(chatty.impact.share), time = time_noun(snapshot));
    let exec = format::percent(chatty.exec_share);
    let scans = chatty.scans.map(|s| s.phrase());
    let basis = crate::support::totals_basis(snapshot, q).filter(|_| chatty.rows_counted_over);
    // "1 row", "1.6 rows", with what they are counted over where that isn't the runs' own basis.
    let rows_said = |rows: f64, one: bool| match (&basis, one) {
        (None, true) => t!("check.chatty_query.rows.one", count = format::count(rows)),
        (None, false) => t!("check.chatty_query.rows.many", count = format::count(rows)),
        (Some(basis), true) => t!("check.chatty_query.rows.one_counted", count = format::count(rows), basis = basis),
        (Some(basis), false) => t!("check.chatty_query.rows.many_counted", count = format::count(rows), basis = basis),
    };
    // Its runs' share of all, then what each run does.
    let runs_share = |key: Verb, rows: Option<String>| -> String {
        let rows = rows.unwrap_or_default();
        match (key, &scans) {
            (Verb::FindsNothingReturns, None) => {
                t!("check.chatty_query.summary.share.poll_returns", tense = tense, share = &exec, rows = rows)
            }
            (Verb::FindsNothingReturns, Some(over)) => t!(
                "check.chatty_query.summary.share.poll_returns_over",
                tense = tense,
                share = &exec,
                over = over,
                rows = rows
            ),
            (Verb::FindsNothingChanges, None) => {
                t!("check.chatty_query.summary.share.poll_changes", tense = tense, share = &exec, rows = rows)
            }
            (Verb::FindsNothingChanges, Some(over)) => t!(
                "check.chatty_query.summary.share.poll_changes_over",
                tense = tense,
                share = &exec,
                over = over,
                rows = rows
            ),
            (Verb::Changes, None) => {
                t!("check.chatty_query.summary.share.changes", tense = tense, share = &exec, rows = rows)
            }
            (Verb::Changes, Some(over)) => t!(
                "check.chatty_query.summary.share.changes_over",
                tense = tense,
                share = &exec,
                over = over,
                rows = rows
            ),
            (Verb::Returns, None) => {
                t!("check.chatty_query.summary.share.returns", tense = tense, share = &exec, rows = rows)
            }
            (Verb::Returns, Some(over)) => t!(
                "check.chatty_query.summary.share.returns_over",
                tense = tense,
                share = &exec,
                over = over,
                rows = rows
            ),
            (Verb::Runs, None) => t!("check.chatty_query.summary.share.runs", tense = tense, share = &exec),
            (Verb::Runs, Some(over)) => {
                t!("check.chatty_query.summary.share.runs_over", tense = tense, share = &exec, over = over)
            }
        }
    };
    // On a source of running totals the counts keep every run from before the change: two scans show it.
    let fewer_runs = |between: String| match verify_between_scans(snapshot, q, &between) {
        Some(scans) => [t!("check.chatty_query.verify.ran", runs = &runs), scans].join(" "),
        None => t!("check.chatty_query.verify.ran_then", runs = &runs, between = between),
    };
    let rows = chatty.rows;
    let engine = snapshot.server.engine;
    let (title, summary, fix, verify, caveat) = match &chatty.shape {
        Shape::FindsNothing { writes } => (
            t!("check.chatty_query.title.finds_nothing", query = &query, tense = tense, runs = &runs),
            [
                runs_share(
                    if *writes { Verb::FindsNothingChanges } else { Verb::FindsNothingReturns },
                    Some(rows_said(rows, rows == 1.0)),
                ),
                t!("check.chatty_query.summary.finds_nothing", tense = tense, share = &share),
            ]
            .join(" "),
            t!("check.chatty_query.fix.finds_nothing"),
            fewer_runs(t!("check.chatty_query.between.finds_nothing")),
            t!("check.chatty_query.caveat.finds_nothing"),
        ),
        Shape::Write(write) => {
            let (summary, fix, verify, caveat) = match write {
                WriteKind::Counter { column, table, isolated } => (
                    [
                        runs_share(Verb::Runs, None),
                        t!("check.chatty_query.summary.counter", column = column, table = table),
                        match lock_waits(snapshot, q) {
                            Some(locks) => {
                                t!("check.chatty_query.summary.counter_locks", share = &share, locks = locks)
                            }
                            None => t!("check.chatty_query.summary.counter_total", share = &share),
                        },
                    ]
                    .join(" "),
                    counter_fix(snapshot, *isolated),
                    fewer_runs(t!("check.chatty_query.between.counter")),
                    t!("check.chatty_query.caveat.counter"),
                ),
                WriteKind::Update => (
                    [
                        runs_share(Verb::Changes, Some(rows_said(rows, rows <= 1.0))),
                        t!("check.chatty_query.summary.update", share = &share),
                    ]
                    .join(" "),
                    t!("check.chatty_query.fix.update", keys = many_keys(engine, "UPDATE")),
                    fewer_runs(t!("check.chatty_query.between.fewer")),
                    t!("check.chatty_query.caveat.update"),
                ),
                WriteKind::Delete => (
                    [
                        runs_share(Verb::Changes, Some(rows_said(rows, rows <= 1.0))),
                        t!("check.chatty_query.summary.delete", share = &share),
                    ]
                    .join(" "),
                    t!("check.chatty_query.fix.delete", keys = many_keys(engine, "DELETE")),
                    fewer_runs(t!("check.chatty_query.between.fewer")),
                    t!("check.chatty_query.caveat.delete"),
                ),
                WriteKind::Insert => (
                    [
                        runs_share(Verb::Changes, Some(rows_said(rows, rows <= 1.0))),
                        t!("check.chatty_query.summary.insert", share = &share),
                    ]
                    .join(" "),
                    t!("check.chatty_query.fix.insert", bulk = bulk_insert(engine)),
                    fewer_runs(t!("check.chatty_query.between.fewer")),
                    t!("check.chatty_query.caveat.insert"),
                ),
            };
            // "One row" only where a run changes exactly one: 1.6 rows a run is said as it is.
            let title = if rows == 1.0 {
                t!("check.chatty_query.title.one_row", query = &query, tense = tense, runs = &runs)
            } else {
                t!(
                    "check.chatty_query.title.rows",
                    query = &query,
                    tense = tense,
                    runs = &runs,
                    rows = if rows <= 1.0 {
                        t!("check.chatty_query.rows.one", count = format::count(rows))
                    } else {
                        t!("check.chatty_query.rows.many", count = format::count(rows))
                    }
                )
            };
            (title, summary, fix, verify, caveat)
        }
        // A read in a loop asks for one key a call, whatever the rows each key finds (1.6 a run is still one lookup).
        Shape::Read => (
            t!("check.chatty_query.title.read", query = &query, tense = tense, runs = &runs),
            [
                runs_share(Verb::Returns, Some(rows_said(rows, rows <= 1.0))),
                t!("check.chatty_query.summary.read", tense = tense, share = &share),
            ]
            .join(" "),
            t!("check.chatty_query.fix.read"),
            fewer_runs(t!("check.chatty_query.between.read")),
            t!("check.chatty_query.caveat.read"),
        ),
    };
    f.title = title;
    f.summary = summary;
    f.severity = chatty.severity;
    f.impact.score = chatty.score;
    f.impact.db_time_share = Some(chatty.impact.share);
    f.impact.time_ms_per_day = chatty.impact.per_day_ms;
    f.impact.label = t!("check.chatty_query.impact", share = &exec);
    f.confidence = Confidence::Medium;
    f.queries = query_refs(&[q]);
    // Named for what they are over, as the title and summary say them: Culprit's scans where they measured it, with
    // the totals' beside where the two differ twofold (never for a recent entry, nor a burst's rate).
    let beside = chatty.scans.filter(|s| !s.is_recent(q)).map(|_| workload.window_phrase());
    match chatty.per_day_runs {
        Some(runs) => f.metrics.push(Metric::new(
            if chatty.scans.is_some() {
                t!("check.chatty_query.metric.runs_per_day_scans")
            } else {
                t!("check.chatty_query.metric.runs_per_day")
            },
            runs,
            Unit::Count,
        )),
        // Runs that came in bursts: what was counted, never a rate a day.
        None if chatty.burst => f.metrics.push(Metric::said(key!("common.metric.runs"), q.executions, Unit::Count)),
        None => {}
    }
    if let (Some(s), Some(totals)) = (&chatty.scans, &beside)
        && s.runs_differ(workload, q)
    {
        f.metrics.push(Metric::new(
            t!("check.chatty_query.metric.runs_per_day_over", over = totals),
            workload.per_day(q.executions),
            Unit::Count,
        ));
    }
    f.metrics.push(Metric::new(
        if chatty.scans.is_some() {
            t!("check.chatty_query.metric.share_scans")
        } else {
            t!("check.chatty_query.metric.share")
        },
        chatty.exec_share,
        Unit::Ratio,
    ));
    if let (Some(s), Some(totals)) = (&chatty.scans, &beside)
        && s.runs_share_differs(workload, q)
    {
        let all = q.executions / workload.total_executions;
        f.metrics.push(Metric::new(t!("check.chatty_query.metric.share_over", over = totals), all, Unit::Ratio));
    }
    f.metrics.extend(average_metrics(snapshot, q));
    f.fix = Some(Fix { summary: fix, sql: None, cost: None });
    f.verify = vec![verify];
    f.caveats.push(caveat);
    f
}

/// What the first sentence of a summary says each run does, beside its share of the runs.
#[derive(Clone, Copy)]
enum Verb {
    /// Most runs find nothing: what a run returns, or changes, on average.
    FindsNothingReturns,
    FindsNothingChanges,
    /// About how many rows each run changes, or returns.
    Changes,
    Returns,
    /// Only its share (a counter, whose runs are said after).
    Runs,
}

/// What a single-row write does, as its text says: the fix for each is different.
#[derive(Debug, PartialEq)]
enum WriteKind {
    /// `UPDATE … SET n = n + 1` returning the new value (`OUTPUT INSERTED.n`, `RETURNING n`, MySQL's
    /// `LAST_INSERT_ID(n + 1)`): a counter handed out one number at a time.
    Counter {
        column: String,
        table: String,
        isolated: bool,
    },
    Update,
    Delete,
    /// INSERT, MERGE, REPLACE, and anything else that writes.
    Insert,
}

impl WriteKind {
    fn of(text: &str, engine: Engine) -> WriteKind {
        let main = main_statement(text);
        match first_verb(main).map(|(_, verb)| verb) {
            Some("update") => match (counter_column(main), write_target(text, engine)) {
                (Some(column), Some(table)) => {
                    let table = table.rsplit('.').next().unwrap_or(&table).to_string();
                    WriteKind::Counter {
                        isolated: culprit_core::sql::isolated_counter_update(text, engine, &column),
                        column,
                        table,
                    }
                }
                _ => WriteKind::Update,
            },
            Some("delete") => WriteKind::Delete,
            _ => WriteKind::Insert,
        }
    }
}

/// The column an UPDATE adds to and returns: `SET [n] = [t].[n] + 1 … OUTPUT INSERTED.[n]`, `SET n = n + 1 RETURNING
/// n`, `SET n += 1 OUTPUT …`, MySQL's `SET n = LAST_INSERT_ID(n + 1)`. None for an update that returns nothing, or
/// changes no column by adding to it.
fn counter_column(text: &str) -> Option<String> {
    let lower = squeeze(text);
    // A keyword as a word: squeezed text writes `where (` as `where(`.
    let keyword = |text: &str, word: &str| {
        text.match_indices(word).map(|(at, _)| at).find(|at| {
            text[at + word.len()..].chars().next().is_none_or(|c| c == ' ' || c == '(') && text[..*at].ends_with(' ')
        })
    };
    let set = keyword(&lower, "set")? + "set".len();
    let rest = &lower[set..];
    // The assignments end where the statement says what it returns, or which rows.
    let end = ["output", "returning", "where", "from"].into_iter().filter_map(|k| keyword(rest, k)).min();
    let assignments = &rest[..end.unwrap_or(rest.len())];
    let returns = ["output", "returning"].into_iter().any(|k| keyword(rest, k).is_some());
    let leaf = |name: &str| name.trim().rsplit('.').next().unwrap_or("").to_string();
    let (mut depth, mut start) = (0i32, 0);
    let mut parts = Vec::new();
    for (i, c) in assignments.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&assignments[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&assignments[start..]);
    parts.into_iter().find_map(|part| {
        if let Some((column, _)) = part.split_once("+=") {
            return returns.then(|| leaf(column));
        }
        let (column, value) = part.split_once('=')?;
        let column = leaf(column);
        let value = value.trim();
        // MySQL hands the new value back through LAST_INSERT_ID(n + 1).
        let (value, handed_back) = match value.strip_prefix("last_insert_id(") {
            Some(inner) => (inner, true),
            None => (value, returns),
        };
        let (first, added) = value.split_once('+')?;
        (handed_back && !column.is_empty() && leaf(first) == column && !added.trim().is_empty()).then_some(column)
    })
}

/// How long a statement waited for locks, where the engine measured it for that statement: Query Store's waits (a
/// statement with waits recorded and none of them on locks waited for none) and MySQL's lock time. PostgreSQL keeps
/// no waits per statement: nothing is said there.
fn lock_waits(snapshot: &Snapshot, q: &QueryStat) -> Option<String> {
    let total = format::duration(q.total_time_ms);
    let locks = || q.waits.iter().filter(|w| w.category == "Lock").map(|w| w.wait_ms).reduce(|a, b| a + b);
    match snapshot.server.engine {
        Engine::Mssql if !q.waits.is_empty() => Some(t!(
            "check.chatty_query.locks.query_store",
            waits = format::duration(locks().unwrap_or(0.0)),
            total = total
        )),
        Engine::Mysql => {
            let lock = q.spread.as_ref().and_then(|s| s.lock_ms).or_else(locks)?;
            Some(t!(
                "check.chatty_query.locks.counted",
                server = snapshot.server.name(),
                waits = format::duration(lock),
                total = total
            ))
        }
        Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Sqlite => None,
    }
}

/// Let the database hand out the numbers: each engine's sequence, without a row to lock; or the number assigned in the
/// INSERT that uses it.
pub(super) fn counter_advice(snapshot: &Snapshot, q: &QueryStat) -> Option<String> {
    match WriteKind::of(&q.text, snapshot.server.engine) {
        WriteKind::Counter { isolated, .. } => Some(counter_fix(snapshot, isolated)),
        _ => None,
    }
}

fn counter_fix(snapshot: &Snapshot, isolated: bool) -> String {
    if !isolated {
        return t!("check.chatty_query.fix.counter_compound");
    }
    let sequence = match snapshot.server.engine {
        Engine::Mssql => t!("check.chatty_query.sequence.mssql"),
        Engine::Postgres | Engine::Yugabyte => t!("check.chatty_query.sequence.postgres"),
        Engine::Mysql if snapshot.server.is_mariadb() => t!("check.chatty_query.sequence.mariadb"),
        Engine::Mysql => t!("check.chatty_query.sequence.mysql"),
        Engine::Sqlite => t!("check.chatty_query.sequence.sqlite"),
    };
    t!("check.chatty_query.fix.counter", sequence = sequence)
}
