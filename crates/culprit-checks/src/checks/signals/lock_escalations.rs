//! The `lock-escalations` check: Tables where one statement took so many row or page locks that SQL Server replaced them with a lock on the whole table (sys.dm_db_index_operational_stats). MySQL, MariaDB and PostgreSQL never escalate row locks.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::Actionability;
use culprit_core::signals::EscalationBaseline;

/// SQL Server considers escalating once one statement holds about this many locks on one index.
pub(super) const ESCALATION_THRESHOLD: f64 = 5_000.0;

/// Escalations a day over a declared native reset window that make a first scan's finding High (one a day: something recurring)
/// or Medium (one a week). Fewer may all be old.
pub(super) const ESCALATIONS_HIGH_PER_DAY: f64 = 1.0;
const ESCALATIONS_MEDIUM_PER_DAY: f64 = 1.0 / 7.0;

pub fn lock_escalations_info() -> CheckInfo {
    CheckInfo {
        id: "lock-escalations".into(),
        title: t!("check.lock_escalations.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.lock_escalations.info.description"),
    }
}

check!(
    LockEscalationsCheck: lock_escalations_info,
    decide = escalated_tables,
    render = lock_escalations_finding,
    case<'s> = EscalatedTable<'s>,
    evidence = &[Evidence::Source(culprit_core::source_id::INDEX_CONTENTION)],
);

#[derive(Default)]
pub(super) struct TableEscalations {
    escalations: f64,
    lock_waits: f64,
    lock_wait_ms: f64,
    reset: Option<culprit_core::ResetMarker>,
}

/// The Extended Events session the lock-escalations fix starts, so the next escalation is kept with its statement.
/// Culprit never starts it; a later Culprit can read it by this name.
pub(crate) const ESCALATION_SESSION: &str = "culprit_lock_escalation";

/// The SQL that starts [`ESCALATION_SESSION`] and reads what it caught: per database on Azure SQL Database, where a
/// session can't see other databases, per server (filtered to `database`) elsewhere.
pub(super) fn escalation_session_sql(database: &str, azure_sql_database: bool) -> String {
    let (sessions, targets, filter, startup) = if azure_sql_database {
        ("sys.dm_xe_database_sessions", "sys.dm_xe_database_session_targets", String::new(), "")
    } else {
        (
            "sys.dm_xe_sessions",
            "sys.dm_xe_session_targets",
            format!("\n    WHERE sqlserver.database_name = N'{}'", database.replace('\'', "''")),
            "\nWITH (STARTUP_STATE = ON)",
        )
    };
    // One session catches the escalations on every table: each table's finding starts the same one, once.
    let start = super::event_session_sql(
        ESCALATION_SESSION,
        azure_sql_database,
        &format!(
            "ADD EVENT sqlserver.lock_escalation (\n    \
                 SET collect_statement = 1\n    \
                 ACTION (sqlserver.sql_text, sqlserver.client_app_name){filter})\n\
             ADD TARGET package0.ring_buffer{startup}"
        ),
    );
    let stop = super::event_session_drop(ESCALATION_SESSION, azure_sql_database);
    format!(
        "-- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.\n\
         -- It catches them on every table, so another table's finding starts this same session: once is enough.\n\
         {start}\n\
         \n\
         -- Then read what it caught\n\
         SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,\n       \
                OBJECT_NAME(e.x.value('(data[@name=\"object_id\"]/value)[1]', 'int')) AS table_name,\n       \
                e.x.value('(data[@name=\"escalated_lock_count\"]/value)[1]', 'int') AS locks,\n       \
                e.x.value('(data[@name=\"statement\"]/value)[1]', 'nvarchar(max)') AS statement,\n       \
                e.x.value('(action[@name=\"client_app_name\"]/value)[1]', 'nvarchar(256)') AS app\n  \
           FROM (SELECT CAST(t.target_data AS xml) AS d\n          \
                   FROM {targets} t\n          \
                   JOIN {sessions} s ON s.address = t.event_session_address\n         \
                  WHERE s.name = N'{ESCALATION_SESSION}' AND t.target_name = N'ring_buffer') r\n \
          CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);\n\
         \n\
         {stop}"
    )
}

/// Why a read keeps its row locks until it ends, which is how a read escalates.
#[derive(Clone, Copy, PartialEq)]
pub enum KeepsLocks {
    /// It asks for a locking hint (HOLDLOCK, UPDLOCK…): kept until its transaction ends.
    Hint,
    /// Its plan prefetches its key lookups on the table: kept until the statement ends.
    Prefetch,
}

/// Why a read of `schema.table` keeps its row locks until the statement ends, which is how a read escalates: under read
/// committed a read gives each row lock up as it moves on, but a plan that prefetches its key lookups on the table
/// (the Nested Loops' `WithUnorderedPrefetch` or `WithOrderedPrefetch`) keeps them, as a locking hint does. Under
/// READ_COMMITTED_SNAPSHOT a read under read committed takes no row locks, so only the hint is left.
fn keeps_row_locks(q: &QueryStat, schema: &str, table: &str, rcsi: bool) -> Option<KeepsLocks> {
    if ["holdlock", "updlock", "xlock", "repeatableread", "serializable"].iter().any(|h| mentions_word(&q.text, h)) {
        return Some(KeepsLocks::Hint);
    }
    let plan = q.plan.as_ref().filter(|_| !rcsi)?;
    let looks_up = plan.nodes().iter().any(|n| {
        n.kind == culprit_core::NodeKind::Lookup && n.object.as_deref().is_some_and(|o| names_table(o, schema, table))
    });
    let prefetches = plan
        .raw
        .as_deref()
        .is_some_and(|raw| raw.contains("WithUnorderedPrefetch=\"1\"") || raw.contains("WithOrderedPrefetch=\"1\""));
    (looks_up && prefetches).then_some(KeepsLocks::Prefetch)
}

/// Counting for less than a day: how long, as every escalation counted is that recent.
#[derive(Clone, Copy)]
pub enum CountedFor {
    Hours(f64),
    Minute,
    Minutes(f64),
}

/// A table whose row or page locks SQL Server escalated to a lock on the whole table, and what the counters establish.
pub struct EscalatedTable<'s> {
    schema: String,
    table: String,
    counts: TableEscalations,
    /// The earlier comparable scan, and the escalations since it.
    new: Option<(&'s EscalationBaseline, f64)>,
    /// None since the earlier scan: history.
    historical: bool,
    /// Without an earlier scan, the escalations a day over a declared reset window, over an hour or more of counting.
    first_scan_rate: Option<f64>,
    /// Counting for less than a day.
    counted_for: Option<CountedFor>,
    /// The writes to the table that change the most rows a run.
    writes: Vec<&'s QueryStat>,
    /// With none, its reads that return the most rows a run, those that keep their row locks first.
    reads: Vec<(&'s QueryStat, Option<KeepsLocks>)>,
    rcsi: bool,
    /// The likeliest write changes thousands of rows a run: the loop is the fix, in its verb.
    bulk: Option<BulkWrite>,
    severity: Severity,
    /// Totals that establish no new increase in the window: said, not graded.
    informational: bool,
}

/// The verb of a write that changes thousands of rows a run, for the batching loop its fix shows.
#[derive(Clone, Copy, PartialEq)]
pub enum BulkWrite {
    Delete,
    Update,
    Other,
}

fn rows_per_run(q: &QueryStat) -> Option<f64> {
    q.rows.map(|r| per_run(r, q.executions))
}

fn escalated_tables(snapshot: &Snapshot) -> Vec<EscalatedTable<'_>> {
    let mut by_table: BTreeMap<(String, String), TableEscalations> = BTreeMap::new();
    for i in &snapshot.indexes {
        let Some(c) = i.contention.as_ref() else { continue };
        // Escalations that happened, never attempts alone: SQL Server's attempt counter moves with its own background
        // work, so on a table of a few dozen rows every index can show an attempt an hour that no statement made.
        if c.lock_escalations <= 0.0 {
            continue;
        }
        let t = by_table.entry((i.schema.clone(), i.table.clone())).or_default();
        t.reset = Some(match t.reset.take() {
            None => c.reset.clone(),
            Some(reset) if reset == c.reset => reset,
            _ => culprit_core::ResetMarker::Unknown,
        });
        t.escalations += c.lock_escalations;
        t.lock_waits += c.lock_waits();
        t.lock_wait_ms += c.lock_wait_ms();
    }
    // SQL Server keeps escalation totals over metadata-cache lifetime and no time for each: only an earlier scan tells the
    // recent ones from old ones.
    let before = snapshot.escalations_before.as_ref();
    let rcsi = snapshot.setting("is_read_committed_snapshot_on").is_some_and(|s| crate::config::is_on(&s.value));
    let mut cases: Vec<EscalatedTable<'_>> = by_table
        .into_iter()
        .map(|((schema, table), counts)| {
            // Counting for less than a day: every escalation counted is that recent ("3 minutes", "5 hours").
            let counted_for =
                counts.reset.as_ref().and_then(|reset| counter_days_since(snapshot, reset)).filter(|d| *d < 1.0).map(
                    |d| match d * 24.0 {
                        h if h >= 1.5 => CountedFor::Hours(h.round()),
                        h => match (h * 60.0).round().max(1.0) {
                            1.0 => CountedFor::Minute,
                            m => CountedFor::Minutes(m),
                        },
                    },
                );
            let name = format!("{schema}.{table}");
            // How many since the earlier scan, when there is one to compare with and the counts compare.
            // A zero delta over only a fraction of the selected window (for example, a scan taken a minute or a few
            // hours ago before selecting 1h, 6h, 24h, 7d or 30d) does not establish that nothing escalated in the window.
            let new = before.and_then(|b| {
                let n = b.since(&name, counts.escalations)?;
                if n == 0.0
                    && b.reference.as_ref().and_then(|r| r.id.as_ref()).is_some()
                    && let Some(minutes) = snapshot.options.as_ref().map(|o| o.window_in_minutes())
                {
                    let hours = days_between(&b.taken_at, &snapshot.taken_at)? * 24.0;
                    if hours < f64::from(minutes) / 60.0 * 0.8 {
                        return None;
                    }
                }
                Some((b, n))
            });
            let historical = new.is_some_and(|(_, n)| n == 0.0);
            // Without one, use only an explicit native reset epoch covering an hour or more.
            let first_scan_rate = if new.is_none() {
                counts.reset.as_ref().and_then(|reset| counter_rate_per_day_since(snapshot, counts.escalations, reset))
            } else {
                None
            };
            // SQL Server doesn't record which statement escalated: the writes to the table that change the most rows
            // per run are the likely ones. Writes to it, not statements that only read it (an INSERT elsewhere that
            // checks it in an EXISTS takes no locks on it to escalate).
            let mut writes: Vec<&QueryStat> =
                app_queries(snapshot).filter(|q| writes_table(q, Engine::Mssql, &schema, &table)).collect();
            writes.sort_by(|a, b| {
                rows_per_run(b)
                    .unwrap_or(0.0)
                    .total_cmp(&rows_per_run(a).unwrap_or(0.0))
                    .then(b.total_time_ms.total_cmp(&a.total_time_ms))
            });
            writes.truncate(3);
            // With no write to it in the query history, a read can be the one: its reads that return the most rows a
            // run, those known to keep their row locks first.
            let mut reads: Vec<(&QueryStat, Option<KeepsLocks>)> = if writes.is_empty() && !historical {
                queries_touching(snapshot, &schema, &table)
                    .into_iter()
                    .filter(|q| !is_monitoring_query(q) && is_read(q))
                    .map(|q| (q, keeps_row_locks(q, &schema, &table, rcsi)))
                    .collect()
            } else {
                Vec::new()
            };
            reads.sort_by(|(a, keeps_a), (b, keeps_b)| {
                keeps_b
                    .is_some()
                    .cmp(&keeps_a.is_some())
                    .then(rows_per_run(b).unwrap_or(0.0).total_cmp(&rows_per_run(a).unwrap_or(0.0)))
            });
            reads.truncate(3);
            // One escalation is a lock on the whole table: every other writer waited for it, however briefly. New ones
            // since the previous scan are High; old ones with none since are history: worth preventing, not an
            // emergency. Without an earlier scan the count keeps no time, so only a measured rate raises it.
            let mut severity = match new {
                Some(_) if historical => Severity::Low,
                Some(_) => Severity::High,
                None => since_start_severity(first_scan_rate, ESCALATIONS_HIGH_PER_DAY, ESCALATIONS_MEDIUM_PER_DAY),
            };
            // A digest's counts cover exactly the time since that digest, however long ago: Friday's, on a Monday.
            let out_of_window = new.is_some_and(|(b, _)| {
                !b.is_from_digest()
                    && !days_between(&b.taken_at, &snapshot.taken_at).is_some_and(|d| {
                        d >= 0.0
                            && snapshot
                                .options
                                .as_ref()
                                .is_none_or(|o| d * 1440.0 <= f64::from(o.window_in_minutes()) * 1.25)
                    })
            });
            let informational = !historical && (out_of_window || (new.is_none() && severity == Severity::Low));
            if informational {
                severity = Severity::Low;
            }
            // The loop is the fix for a statement that changes thousands of rows at once. When every write in the
            // query history changes a few rows a run, the one that escalated isn't among them: finding it comes first.
            let bulk =
                writes.first().filter(|q| rows_per_run(q).is_some_and(|r| r >= ESCALATION_THRESHOLD / 5.0)).map(|q| {
                    match first_verb(main_statement(&q.text)).map(|(_, v)| v) {
                        Some("delete") => BulkWrite::Delete,
                        Some("update") => BulkWrite::Update,
                        _ => BulkWrite::Other,
                    }
                });
            EscalatedTable {
                schema,
                table,
                counts,
                new,
                historical,
                first_scan_rate,
                counted_for,
                writes,
                reads,
                rcsi,
                bulk,
                severity,
                informational,
            }
        })
        .collect();
    cases.sort_by(|a, b| {
        base_score(b.severity)
            .total_cmp(&base_score(a.severity))
            .then(format!("{}.{}", a.schema, a.table).cmp(&format!("{}.{}", b.schema, b.table)))
    });
    cases
}

/// Rows a run as whole rows past a handful and to a tenth below it, so 252 rows in 257 runs reads "1 row" next to
/// another statement's "1 row", not "1.0 rows".
fn rows_words(rows: f64) -> String {
    let n = if rows >= 10.0 { rows.round() } else { (rows * 10.0).round() / 10.0 };
    t!("check.lock_escalations.rows", n = n, count = format::count(n))
}

fn lock_escalations_finding(snapshot: &Snapshot, e: EscalatedTable<'_>) -> Finding {
    let EscalatedTable { schema, table, counts: t, new, historical, .. } = e;
    let name = format!("{schema}.{table}");
    let mut f = finding("lock-escalations", &name, Category::Indexes, subject(SubjectKind::Table, &name));
    let started = t.reset.as_ref().and_then(|reset| reset.at());
    let server_started_on = snapshot.server.started_at.as_deref().map(crate::queries::when_phrase);
    let azure = snapshot.server.on(culprit_core::Platform::AzureSqlDatabase);
    let since_start = match started {
        Some(at) => t!("check.lock_escalations.since_started", date = format::friendly_date(at, &snapshot.taken_at)),
        None => t!("check.lock_escalations.since_loaded"),
    };
    let started_on = started.map(crate::queries::when_phrase);
    f.verification_basis = culprit_core::VerificationBasis::CounterChange;
    f.facts.advice.diagnostic_cleanup = true;
    if historical {
        f.actionability = Actionability::History;
    }
    let (total, total_n) = (format::count(t.escalations), t.escalations);
    let mut summary = Vec::new();
    match new {
        Some((b, n)) if n > 0.0 => {
            f.title = t!(
                "check.lock_escalations.title.new",
                table = &table,
                n = n,
                count = format::count(n),
                since = crate::queries::short_when(&b.taken_at)
            );
            summary.push(t!(
                "check.lock_escalations.summary.new",
                table = &table,
                n = n,
                count = format::count(n),
                when = crate::queries::when_phrase(&b.taken_at)
            ));
            summary.push(if b.restarted {
                match server_started_on.as_deref() {
                    Some(on) => t!("check.lock_escalations.summary.restarted_on", on = on),
                    None => t!("check.lock_escalations.summary.restarted"),
                }
            } else if n < t.escalations {
                t!("check.lock_escalations.summary.counted_in_all", total = &total, since = &since_start)
            } else {
                t!("check.lock_escalations.summary.all_after", total = &total, since = &since_start)
            });
        }
        Some((b, _)) => {
            f.title = t!(
                "check.lock_escalations.title.none_since",
                table = &table,
                n = total_n,
                count = &total,
                since_start = &since_start,
                since = crate::queries::short_when(&b.taken_at)
            );
            let when = crate::queries::when_phrase(&b.taken_at);
            summary.push(match started_on.as_deref() {
                Some(on) => {
                    t!("check.lock_escalations.summary.history_started_on", on = on, table = &table, when = when)
                }
                None => t!("check.lock_escalations.summary.history", table = &table, when = when),
            });
        }
        None => {
            f.title = t!(
                "check.lock_escalations.title.first",
                table = &table,
                n = total_n,
                count = &total,
                since = &since_start
            );
            let since = started_on
                .as_deref()
                .map(|on| t!("check.lock_escalations.since_it_started", on = on))
                .unwrap_or_else(|| since_start.clone());
            summary.push(match e.counted_for {
                Some(counted) => {
                    let ago = match counted {
                        CountedFor::Hours(h) => t!("check.lock_escalations.ago.hours", hours = format!("{h}")),
                        CountedFor::Minute => t!("check.lock_escalations.ago.minute"),
                        CountedFor::Minutes(m) => t!("check.lock_escalations.ago.minutes", minutes = format!("{m}")),
                    };
                    t!(
                        "check.lock_escalations.summary.first_recent",
                        table = &table,
                        n = total_n,
                        count = &total,
                        since = &since,
                        ago = ago
                    )
                }
                None => {
                    t!(
                        "check.lock_escalations.summary.first",
                        table = &table,
                        n = total_n,
                        count = &total,
                        since = &since
                    )
                }
            });
            // Without an earlier scan, only how often they happened on average says whether they recur.
            summary.push(match e.first_scan_rate {
                Some(r) if r >= ESCALATIONS_HIGH_PER_DAY => t!("check.lock_escalations.summary.daily"),
                Some(r) if r >= ESCALATIONS_MEDIUM_PER_DAY => t!("check.lock_escalations.summary.weekly"),
                Some(_) => t!("check.lock_escalations.summary.rarely"),
                None if e.counted_for.is_some() => t!("check.lock_escalations.summary.too_recent"),
                None => t!("check.lock_escalations.summary.unknown_window"),
            });
        }
    }
    summary.push(t!("check.lock_escalations.summary.attempts"));
    let writes = &e.writes;
    let reads = &e.reads;
    summary.push(if historical {
        if writes.is_empty() {
            t!("check.lock_escalations.suspects.history_no_writes")
        } else {
            t!("check.lock_escalations.suspects.history_writes")
        }
    } else {
        match writes.first() {
            Some(q) => match rows_per_run(q) {
                Some(rows) if rows >= ESCALATION_THRESHOLD / 5.0 => t!(
                    "check.lock_escalations.suspects.likeliest_write",
                    handle = query_handle(q),
                    rows = rows_words(rows)
                ),
                _ => t!("check.lock_escalations.suspects.writes"),
            },
            None if !reads.is_empty() => {
                let mut said = vec![
                    t!("check.lock_escalations.suspects.no_writes", table = &table),
                    if e.rcsi {
                        t!("check.lock_escalations.suspects.reads_rcsi")
                    } else {
                        t!("check.lock_escalations.suspects.reads")
                    },
                ];
                said.push(match reads.first() {
                    Some((q, Some(keeps))) => {
                        let rows = rows_per_run(q)
                            .map(rows_words)
                            .unwrap_or_else(|| t!("check.lock_escalations.rows_unknown"));
                        match keeps {
                            KeepsLocks::Hint => t!(
                                "check.lock_escalations.suspects.likeliest_read_hint",
                                handle = query_handle(q),
                                rows = rows
                            ),
                            KeepsLocks::Prefetch => t!(
                                "check.lock_escalations.suspects.likeliest_read_prefetch",
                                handle = query_handle(q),
                                rows = rows
                            ),
                        }
                    }
                    _ => t!("check.lock_escalations.suspects.reads_listed"),
                });
                said.join(" ")
            }
            None => t!("check.lock_escalations.suspects.none"),
        }
    });
    f.summary = summary.join(" ");
    f.queries = writes
        .iter()
        .map(|q| QueryRef {
            id: q.id.clone(),
            plan_node: None,
            note: rows_per_run(q).map(|rows| t!("check.lock_escalations.note.changes", rows = rows_words(rows))),
        })
        .chain(reads.iter().map(|(q, keeps)| QueryRef {
            id: q.id.clone(),
            plan_node: None,
            note: rows_per_run(q).map(|rows| match keeps {
                Some(KeepsLocks::Hint) => t!("check.lock_escalations.note.returns_hint", rows = rows_words(rows)),
                Some(KeepsLocks::Prefetch) => {
                    t!("check.lock_escalations.note.returns_prefetch", rows = rows_words(rows))
                }
                None => t!("check.lock_escalations.note.returns", rows = rows_words(rows)),
            }),
        }))
        .collect();
    f.last_seen = writes.first().or(reads.first().map(|(q, _)| q)).and_then(|q| q.last_seen.clone());
    f.severity = e.severity;
    if e.informational {
        f.informational();
        f.caveats.push(t!("check.lock_escalations.caveat.no_new_increase"));
    }
    severity_score(&mut f);
    f.impact.set_label(match new {
        Some((_, n)) if n > 0.0 => t!("check.lock_escalations.impact.new", n = n, count = format::count(n)),
        Some(_) => t!("check.lock_escalations.impact.none_new", n = total_n, count = &total),
        None => t!("check.lock_escalations.impact.total", n = total_n, count = &total),
    });
    f.confidence = Confidence::High;
    f.metrics = vec![Metric::said(super::ESCALATIONS_METRIC, t.escalations, Unit::Count)];
    if let Some((_, n)) = new {
        f.metrics.push(Metric::said(key!("check.lock_escalations.metric.since_previous"), n, Unit::Count));
    }
    if t.lock_waits >= 1.0 {
        f.metrics.push(Metric::said(
            culprit_core::key!("check.lock_escalations.metric.lock_waits"),
            t.lock_waits,
            Unit::Count,
        ));
        f.metrics.push(Metric::said(key!("check.lock_escalations.metric.time_waiting"), t.lock_wait_ms, Unit::Ms));
    }
    let target = qualified(Engine::Mssql, &schema, &table);
    let session = escalation_session_sql(&snapshot.server.database, azure);
    // The loop in the likeliest statement's own verb: a delete or an update; anything else is the reader's to batch.
    let lp = match e.bulk {
        Some(BulkWrite::Delete) => Some(format!("DELETE TOP (1000) FROM {target} WHERE /* the same filter */;")),
        Some(BulkWrite::Update) => Some(format!(
            "UPDATE TOP (1000) {target} SET /* the same changes */ WHERE /* the same filter, leaving out rows already changed */;"
        )),
        _ => None,
    };
    f.fix = Some(Fix {
        summary: match reads.first() {
            // A read keeps its locks for a reason of its own; the loop is for a write the query history doesn't show.
            Some((_, keeps)) => t!(
                "check.lock_escalations.fix.read",
                session = ESCALATION_SESSION,
                rcsi = if e.rcsi { "on" } else { "off" },
                hint = if *keeps == Some(KeepsLocks::Hint) { "yes" } else { "no" }
            ),
            None if historical => {
                t!("check.lock_escalations.fix.loop_history", table = &table, session = ESCALATION_SESSION)
            }
            None if e.bulk.is_some() => {
                t!("check.lock_escalations.fix.loop_bulk", table = &table, session = ESCALATION_SESSION)
            }
            None => t!("check.lock_escalations.fix.loop_unknown", table = &table, session = ESCALATION_SESSION),
        },
        sql: Some(match lp {
            Some(statement) => format!(
                "-- Example only: check lock footprint and required atomicity before batching\n\
                 -- WHILE 1 = 1\n\
                 -- BEGIN\n\
                 --     {statement}\n\
                 --     IF @@ROWCOUNT = 0 BREAK;\n\
                 -- END;\n\
                 \n\
                 {session}"
            ),
            None => session,
        }),
        cost: None,
    });
    f.verify = vec![t!("check.lock_escalations.verify", table = &table)];
    f.caveats.push(t!("check.lock_escalations.caveat.counted_since_loaded"));
    f
}
