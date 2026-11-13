//! The `unstable-query` check: Queries whose duration swings wildly between runs with the same plan, a sign of parameter sensitivity or blocking.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::{ScanSpan, t};

mod recorded;
use recorded::Recorded;

pub fn unstable_query_info() -> CheckInfo {
    CheckInfo {
        id: "unstable-query".into(),
        title: t!("check.unstable_query.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![],
        description: t!("check.unstable_query.info.description"),
    }
}

check!(
    UnstableQueryCheck: unstable_query_info,
    decide = unstable_queries,
    render = unstable_query_finding,
    case<'s> = UnstableQuery<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Variance)],
);

/// How far a statement's run times spread: the standard deviation over the average, and the slowest run over it.
pub(super) const UNSTABLE_CV: f64 = 3.0;
const UNSTABLE_MAX_FACTOR: f64 = 50.0;

/// The slow runs behind a wide spread, when it takes more than one slow run to explain it. One outlier among many fast
/// runs (a lock wait during a migration, a cold cache after a restart) inflates the standard deviation on its own: the
/// spread has to stay wide without the slowest run's share of it. Returns the least time the slow runs spent beyond
/// the average (ms): every slow run is at most the slowest one, so the variance they explain needs at least that much.
pub(super) fn unstable_excess_ms(q: &QueryStat) -> Option<f64> {
    let (sd, max, n, mean) = (q.stddev_time_ms?, q.max_time_ms?, q.executions, q.mean_time_ms);
    if n < 50.0 || mean <= 0.0 || sd / mean < UNSTABLE_CV || max < mean * UNSTABLE_MAX_FACTOR || max < 1000.0 {
        return None;
    }
    let variance = sd * sd;
    let rest_variance = variance - (max - mean).powi(2) / n;
    let rest_mean = (mean * n - max) / (n - 1.0);
    if rest_variance <= 0.0 || rest_mean <= 0.0 || rest_variance.sqrt() / rest_mean < UNSTABLE_CV {
        return None;
    }
    // Σ(x − mean)² over the slow runs ≤ (max − mean)·Σ(x − mean); the fast runs add at most mean² each.
    let excess = ((variance - mean * mean).max(0.0) * n / (max - mean)).min(q.total_time_ms);
    // Each slow run is at most the slowest, so it took at least excess / (max − mean) of them: two runs that hit one
    // blocking incident aren't a statement whose runs swing, however wide they make the spread.
    (excess > 0.0 && excess / (max - mean) >= crate::queries::MIN_TAIL_RUNS).then_some(excess)
}

/// A statement whose run times swing on one plan, and what its numbers establish about why.
pub struct UnstableQuery<'s> {
    query: &'s QueryStat,
    impact: QueryImpact,
    stddev: f64,
    max: f64,
    /// The slow runs' time beyond the average (ms), at least.
    excess: f64,
    /// That as a share of the workload, and a day where the window says so.
    slow_share: f64,
    time_ms_per_day: Option<f64>,
    severity: Severity,
    /// It stopped running: told in the past.
    stopped: bool,
    /// What Culprit's scans measured of it, where they did.
    scans: Option<ScanSpan>,
    /// A statement that writes the values it is given: what its runs can have waited for.
    waited: Option<WriteWaits<'s>>,
    /// What the engine recorded of its waits.
    recorded: Recorded,
    /// The fewest slow runs that add up to the extra time.
    fewest: f64,
}

/// The unstable-query findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn unstable_query(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&UnstableQueryCheck, snapshot)
}

fn unstable_queries(snapshot: &Snapshot) -> Vec<UnstableQuery<'_>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    app_queries(snapshot)
        .filter_map(|q| {
            if q.workload_plans().len() > 1 {
                return None;
            }
            let (stddev, max) = (q.stddev_time_ms?, q.max_time_ms?);
            let excess = unstable_excess_ms(q)?;
            let impact = query_impact(snapshot, &[q])?;
            let slow_share = workload.share_of(excess, q);
            if impact.share < 0.01 || slow_share < 0.005 {
                return None;
            }
            // A visible values-only write narrows the diagnostic possibilities. Waits, triggers and constraints
            // can do work absent from this plan, so it does not prove that every slow run was waiting.
            let waited = (snapshot.server.engine.has_server() && q.plan.as_ref().is_some_and(writes_without_reading))
                .then(|| WriteWaits::read(snapshot, q, excess));
            Some(UnstableQuery {
                query: q,
                impact,
                stddev,
                max,
                excess,
                slow_share,
                // What the slow runs cost, not the whole statement's time: the share and the time a day shown beside
                // the finding (its row, the dashboard, the digest, an agent's view) are what its label says, and so are
                // the grade and the rank. The statement's own share is on the statement (a brief's "Queries
                // involved").
                time_ms_per_day: workload.per_day_measured(excess).filter(|_| workload.runs_per_day_of(q).is_some()),
                severity: severity_for_time_of(snapshot, &[q], excess, slow_share),
                stopped: crate::all_stopped(snapshot, &[q]),
                scans: workload.scans_of(q),
                waited,
                // Aggregate waits retain observed candidates without assigning them to particular slow calls.
                recorded: Recorded::read(snapshot, q),
                fewest: (excess / (max - q.mean_time_ms).max(1.0)).ceil(),
            })
        })
        .collect()
}

fn unstable_query_finding(snapshot: &Snapshot, case: UnstableQuery<'_>) -> Finding {
    let q = case.query;
    let workload = snapshot.workload.as_ref().expect("the check needs a workload");
    let mut f = finding("unstable-query", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    apply_impact(snapshot, &mut f, &case.impact);
    f.impact.db_time_share = Some(case.slow_share);
    f.impact.time_ms_per_day = case.time_ms_per_day;
    f.severity = case.severity;
    f.impact.score = score_for_share(case.slow_share);
    f.impact.set_label(if slow_log_only(snapshot) {
        t!("check.unstable_query.impact.logged", share = format::percent(case.slow_share))
    } else {
        t!("check.unstable_query.impact", share = format::percent(case.slow_share))
    });
    f.confidence = Confidence::Medium;
    // A statement that stopped running is told in the past. Its average as every surface states it (over Culprit's
    // scans where they measured it); the spread below is the source's own, over its totals.
    let tense = if case.stopped { "past" } else { "present" };
    f.title = t!(
        "check.unstable_query.title",
        query = capitalize(&query_handle(q)),
        tense = tense,
        mean = format::duration(workload.statement_mean_ms(q)),
        max = format::duration(case.max)
    );
    // One plan in Query Store backs "same plan"; elsewhere the engine keeps no plan identity per run.
    let same = if q.workload_plans().len() == 1 { "plan" } else { "statement" };
    let excess = format::duration(case.excess);
    // The spread is the source's, over its totals: where the title's average (Culprit's scans') is twice or half
    // theirs, the one the slow runs are measured against is named; the excess is the totals' otherwise.
    let spread = match (case.scans.filter(|s| s.mean_differs(q)), crate::support::totals_basis(snapshot, q)) {
        (Some(s), _) => t!(
            "check.unstable_query.summary.spread_scans",
            same = same,
            excess = excess,
            mean = format::duration(q.mean_time_ms),
            window = workload.window_phrase(),
            scans_mean = format::duration(s.mean_ms().unwrap_or(q.mean_time_ms)),
            scans = s.phrase()
        ),
        (None, Some(basis)) => {
            t!("check.unstable_query.summary.spread_basis", same = same, excess = excess, basis = basis)
        }
        (None, None) => t!("check.unstable_query.summary.spread", same = same, excess = excess),
    };
    let why = if let Some(w) = &case.waited {
        w.summary(snapshot)
    } else if snapshot.server.engine.has_server() {
        case.recorded.sentence(snapshot, case.excess)
    } else {
        // SQLite: no server to record waits; a run that waits is waiting for the file's write lock.
        t!("check.unstable_query.summary.sqlite")
    };
    f.summary = [spread, why].join(" ");
    f.queries = query_refs(&[q]);
    // As the title states it, named over Culprit's scans where they measured it; where those measured one twice or half
    // the spread's own (since the totals began), that one beside it.
    match case.scans {
        Some(s) => {
            f.metrics.push(Metric::said(
                culprit_core::key!("check.unstable_query.metric.average_scans"),
                workload.statement_mean_ms(q),
                Unit::Ms,
            ));
            if s.mean_differs(q) {
                f.metrics.push(Metric::new(
                    t!("check.unstable_query.metric.average_over", window = workload.window_phrase()),
                    q.mean_time_ms,
                    Unit::Ms,
                ));
            }
        }
        None => f.metrics.push(Metric::said(
            culprit_core::key!("check.unstable_query.metric.average"),
            workload.statement_mean_ms(q),
            Unit::Ms,
        )),
    }
    f.metrics.push(Metric::said(culprit_core::key!("check.unstable_query.metric.slowest"), case.max, Unit::Ms));
    f.metrics.push(Metric::said(culprit_core::key!("check.unstable_query.metric.stddev"), case.stddev, Unit::Ms));
    // A part of its time the source keeps only in its totals: over those, where the average beside it is the scans'.
    let extra = match crate::support::totals_basis(snapshot, q) {
        Some(basis) => t!("check.unstable_query.metric.extra_over", basis = basis),
        None => t!("check.unstable_query.metric.extra"),
    };
    f.metrics.push(Metric::new(extra, case.excess, Unit::Ms));
    if let Some(io) = case.waited.as_ref().and_then(|w| w.io_ms) {
        f.metrics.push(Metric::said(culprit_core::key!("check.unstable_query.metric.io"), io, Unit::Ms));
    }
    // Parameter comparisons remain an investigation; aggregate waits do not select a cause.
    let mut fix = match (&case.waited, snapshot.server.engine) {
        (Some(w), Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite) => {
            w.fix(snapshot)
        }
        (None, Engine::Mssql) => {
            f.facts.advice.application = culprit_core::ApplicationAdvice::Alternative;
            crate::queries::mssql_fix(snapshot, q, None, case.excess)
        }
        (None, Engine::Postgres) => crate::queries::postgres_fix(),
        (None, Engine::Yugabyte) => Fix { summary: t!("check.unstable_query.fix.yugabyte"), sql: None, cost: None },
        (None, Engine::Mysql) => {
            // There is no proven allocation of the aggregate lock total to the slow calls.
            crate::queries::mysql_fix(snapshot, q, 0.0, Some(case.fewest))
        }
        (None, Engine::Sqlite) => {
            Fix { summary: t!("check.unstable_query.fix.sqlite"), sql: Some("ANALYZE;".into()), cost: None }
        }
    };
    if case.waited.is_none() && case.recorded.has_positive_locks() {
        // Aggregate lock totals do not identify the slow calls. Preserve both investigations rather than forcing
        // the lock-only or parameter-only ladder, and do not generate a guessed owner's command.
        fix.summary =
            [t!("check.unstable_query.fix.watch"), t!("check.unstable_query.fix.inspect"), fix.summary].join(" ");
    }
    f.fix = Some(fix);
    f.verify = vec![verify_new_runs(snapshot)];
    if snapshot.server.engine.sql_syntax() == culprit_core::SqlSyntax::Postgres {
        // A plan that reads no table has no values to blame: which ones were slow doesn't matter.
        let unknown = if case.waited.is_some() { "calls" } else { "values" };
        let engine = snapshot.server.engine.label();
        f.caveats.push(match q.first_seen.as_deref() {
            Some(at) => t!(
                "check.unstable_query.caveat.since",
                engine = engine,
                since = crate::queries::short_when(at),
                unknown = unknown
            ),
            None => t!(
                "check.unstable_query.caveat.window",
                engine = engine,
                window = workload.window_phrase(),
                unknown = unknown
            ),
        });
    }
    f
}

/// A spread must be checked on new runs: cumulative extrema and variance cannot isolate the runs after a fix.
fn verify_new_runs(snapshot: &Snapshot) -> String {
    let workload = snapshot.workload.as_ref().expect("the check needs a workload");
    let compare = t!("check.unstable_query.verify.compare");
    match workload.kind() {
        Some(culprit_core::WorkloadKind::QueryStore) => {
            [t!("check.unstable_query.verify.query_store"), compare].join(" ")
        }
        Some(culprit_core::WorkloadKind::PlanCache) => [
            t!("check.unstable_query.verify.plan_cache"),
            compare,
            t!("check.unstable_query.verify.plan_cache_totals", source = workload.source_name()),
        ]
        .join(" "),
        Some(culprit_core::WorkloadKind::SlowLog | culprit_core::WorkloadKind::QueryLog) => {
            [t!("check.unstable_query.verify.log"), compare].join(" ")
        }
        _ => {
            let log = if snapshot.server.engine == Engine::Mysql {
                t!("check.unstable_query.verify.slow_query_log")
            } else {
                t!("check.unstable_query.verify.server_log")
            };
            [
                t!("check.unstable_query.verify.capture", log = log),
                compare,
                t!("check.unstable_query.verify.totals", source = workload.source_name()),
            ]
            .join(" ")
        }
    }
}

/// What the runs of a statement that writes the values it is given can have waited for, from the catalog and the
/// statement's own counters: the rows its foreign keys check, its data-file time, and, once it stopped, what writes
/// the same table now.
struct WriteWaits<'a> {
    /// Its foreign keys, when it inserts: each new row is checked against the referenced row, which is read and
    /// share-locked.
    keys: Vec<&'a culprit_core::ForeignKey>,
    /// The unique index whose columns its column list names (the primary key first): every run looks its key up there
    /// before it writes, a read its plan doesn't show.
    key_index: Option<&'a str>,
    /// Whether another transaction can insert the key it inserts: the table has a unique index whose columns its
    /// column list names (a key the database generates, an identity or a sequence, never repeats). Unknown is true.
    same_key: bool,
    /// The share of its runs that inserted nothing: an insert of one row that skips a key already there (`ON CONFLICT
    /// DO NOTHING`, `INSERT IGNORE`), counted from the rows it inserted.
    inserted_nothing: Option<f64>,
    /// Its data-file reads and writes, when the engine timed them and they are a small part of the slow runs' time.
    io_ms: Option<f64>,
    /// The slow runs' extra time (ms), at least.
    excess: f64,
    /// The fewest slow runs that add up to it: each is at most the slowest run.
    fewest_slow: Option<f64>,
    /// The average run (ms), to tell a slow run from the others.
    mean_ms: f64,
    /// It stopped running (see `query_stopped`).
    stopped: bool,
    /// Once it stopped: a statement still running that writes the same table the same way, what may have replaced it.
    successor: Option<&'a QueryStat>,
}

/// A small recorded I/O total is context, not proof that every historical run was timed.
const SMALL_RECORDED_IO: f64 = 0.1;

impl<'a> WriteWaits<'a> {
    fn read(snapshot: &'a Snapshot, q: &QueryStat, excess: f64) -> Self {
        let engine = snapshot.server.engine;
        let table = write_targets(q, engine)
            .into_iter()
            .find_map(|o| snapshot.tables.iter().find(|t| names_table(&o, &t.schema, &t.name)));
        let verb = first_verb(main_statement(&q.text)).map(|(_, v)| v);
        let keys = match table {
            Some(t) if verb == Some("insert") => snapshot
                .foreign_keys
                .iter()
                .filter(|k| k.schema.eq_ignore_ascii_case(&t.schema) && k.table.eq_ignore_ascii_case(&t.name))
                .collect(),
            _ => Vec::new(),
        };
        let (same_key, key_index) = match (table, verb) {
            // No index listed for it at all is no index read, not a table without keys.
            (Some(t), Some("insert")) if snapshot.indexes_on(&t.schema, &t.name).next().is_some() => {
                let key = inserted_unique_key(snapshot, q, t);
                (key.is_some(), key.map(|i| i.name.as_str()))
            }
            _ => (true, None),
        };
        // One row a run, skipped when its key is there: the rows it inserted say how many runs wrote nothing.
        let squeezed = squeeze(&q.text);
        let skips_existing = (squeezed.contains(" on conflict") && squeezed.contains(" do nothing"))
            || squeezed.starts_with("insert ignore ");
        let inserted_nothing = q
            .rows_handled_per_execution()
            .filter(|rows| skips_existing && values_rows(main_statement(&q.text)) == Some(1) && *rows < 0.5)
            .map(|rows| 1.0 - rows);
        let fewest_slow = q.max_time_ms.map(|max| excess / (max - q.mean_time_ms).max(1.0));
        // Only a time the engine measured: no entry is no measurement, not zero.
        let io_ms = q
            .waits
            .iter()
            .filter(|w| w.category == "IO")
            .map(|w| w.wait_ms)
            .reduce(|a, b| a + b)
            .filter(|io| *io < excess * SMALL_RECORDED_IO);
        let stopped = crate::query_stopped(snapshot, q).is_some();
        let successor = stopped.then(|| crate::refine::query_successor(snapshot, q)).flatten();
        WriteWaits {
            keys,
            key_index,
            same_key,
            inserted_nothing,
            io_ms,
            excess,
            fewest_slow,
            mean_ms: q.mean_time_ms,
            stopped,
            successor,
        }
    }

    /// The tables its foreign keys check, as a sentence names them: "apps", "apps and users".
    fn parents(&self) -> String {
        let mut names: Vec<String> = Vec::new();
        for k in &self.keys {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&k.ref_table)) {
                names.push(k.ref_table.clone());
            }
        }
        format::list(&names)
    }

    /// Why its runs swing and what they may have waited for: a guess the engine's numbers narrow, said as one.
    fn summary(&self, snapshot: &Snapshot) -> String {
        let engine = snapshot.server.engine;
        // What each run reads that its plan doesn't show: its key in the unique index, and the rows its foreign keys
        // check.
        let checks = (!self.keys.is_empty()).then(|| {
            let columns: Vec<String> = self.keys.iter().map(|k| k.columns.join(", ")).collect();
            let (columns, parents) = (format::list(&columns), self.parents());
            match (self.keys.len() == 1, engine.sql_syntax() == culprit_core::SqlSyntax::Postgres) {
                (true, true) => t!("check.unstable_query.checks.one_key_share", columns = columns, parents = parents),
                (true, false) => t!("check.unstable_query.checks.one", columns = columns, parents = parents),
                (false, true) => t!("check.unstable_query.checks.many_key_share", columns = columns, parents = parents),
                (false, false) => t!("check.unstable_query.checks.many", columns = columns, parents = parents),
            }
        });
        let mut said = vec![match (self.key_index, checks) {
            (None, None) => t!("check.unstable_query.reads.none"),
            (None, Some(checks)) => t!("check.unstable_query.reads.checks", checks = checks),
            (Some(index), None) => t!("check.unstable_query.reads.key", index = index),
            (Some(index), Some(checks)) => t!("check.unstable_query.reads.key_checks", index = index, checks = checks),
        }];
        if let Some(nothing) = self.inserted_nothing {
            said.push(t!("check.unstable_query.inserted_nothing", share = format::percent(nothing)));
        }
        if let Some(io) = self.io_ms {
            said.push(t!(
                "check.unstable_query.recorded_io",
                io = format::duration(io),
                excess = format::duration(self.excess)
            ));
        }
        let mut waits: Vec<String> = Vec::new();
        if !self.keys.is_empty() {
            waits.push(t!("check.unstable_query.wait.transaction", referenced = self.referenced(engine)));
        }
        if self.same_key {
            waits.push(t!("check.unstable_query.wait.same_key"));
        }
        waits.push(t!("check.unstable_query.wait.table_lock"));
        waits.push(self.storage(engine));
        let last = waits.pop().unwrap_or_default();
        said.push(t!("check.unstable_query.waiting", waits = waits.join(", "), last = last));
        said.join(" ")
    }

    /// The storage a run can wait on, as this engine times a statement. PostgreSQL's statement time is the executor's:
    /// an autocommit statement's commit, and the WAL flush it waits for, come after it. SQL Server's Query Store and
    /// MySQL's statement events time an autocommit statement's commit with it.
    fn storage(&self, engine: Engine) -> String {
        match engine {
            Engine::Postgres => t!("check.unstable_query.wait.storage_checkpoint"),
            Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
                t!("check.unstable_query.wait.storage_commit")
            }
        }
    }

    /// What holds the referenced row a foreign key checks, the same words in the summary and the fix: PostgreSQL's
    /// FOR KEY SHARE waits only for a row lock, a delete or a change of the key; the others' shared lock waits for any
    /// change of the row.
    fn referenced(&self, engine: Engine) -> String {
        match engine {
            Engine::Postgres | Engine::Yugabyte => {
                t!("check.unstable_query.referenced.key_share", parents = self.parents())
            }
            Engine::Mssql | Engine::Mysql | Engine::Sqlite => {
                t!("check.unstable_query.referenced.shared", parents = self.parents())
            }
        }
    }

    /// Where the slow runs are too few to catch while they happen: the fewest that add up to the extra time come less
    /// than once an hour over the data's window. Their count, and the window as a sentence names it.
    fn too_rare_to_watch(&self, snapshot: &Snapshot) -> Option<(f64, String)> {
        let w = snapshot.workload.as_ref()?;
        let fewest = self.fewest_slow?.ceil();
        too_rare_to_catch(snapshot, fewest).then(|| (fewest, w.window_phrase()))
    }

    /// How to see the slow runs Live is unlikely to catch: the engine's log of slow statements and lock waits, set the
    /// way settings are set on this server (with its SQL, where it runs), or what Query Store already keeps; the one
    /// way every check that names a wait too rare to catch says it ([`log_next_waits`]).
    fn record_slow_runs(&self, snapshot: &Snapshot, mean_ms: f64) -> Option<(String, Option<String>)> {
        // A run well past the average, and no lower than a tenth of a second: what the log keeps.
        let bar = [100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0]
            .into_iter()
            .find(|b| *b >= mean_ms * 20.0)
            .unwrap_or(10_000.0);
        match snapshot.server.engine {
            Engine::Mssql => Some((t!("check.unstable_query.fix.query_store_waits"), None)),
            Engine::Sqlite => None,
            Engine::Postgres | Engine::Yugabyte | Engine::Mysql => {
                let LockHolder { lead, sql } = log_next_waits(snapshot, Some(bar));
                Some((lead, sql))
            }
        }
    }

    /// What to do: watch the waits while it runs, have the next slow runs logged where they are too rare to watch,
    /// or, once it stopped, what took its place.
    fn fix(&self, snapshot: &Snapshot) -> Fix {
        if self.stopped {
            let mut summary = vec![t!("check.unstable_query.fix.stopped")];
            if let Some(q) = self.successor {
                summary.push(t!("check.unstable_query.fix.successor", query = &q.id, handle = query_handle(q)));
                if let Some(ms) = q.max_time_ms {
                    summary.push(t!("check.unstable_query.fix.successor_slowest", slowest = format::duration(ms)));
                }
            }
            summary.push(t!("check.unstable_query.fix.historical"));
            return Fix { summary: summary.join(" "), sql: None, cost: None };
        }
        let engine = snapshot.server.engine;
        let rare = self
            .too_rare_to_watch(snapshot)
            .and_then(|(fewest, window)| Some((fewest, window, self.record_slow_runs(snapshot, self.mean_ms)?)));
        let mut sql = None;
        let mut summary = match rare {
            Some((fewest, window, (record, record_sql))) => {
                sql = record_sql;
                vec![t!("check.unstable_query.fix.too_rare", count = format::count(fewest), window = window), record]
            }
            None => vec![t!("check.unstable_query.fix.watch")],
        };
        let referenced = self.referenced(engine);
        summary.push(match (self.same_key, self.keys.is_empty()) {
            (true, true) => t!("check.unstable_query.fix.blockers.same_key"),
            (true, false) => t!("check.unstable_query.fix.blockers.same_key_referenced", referenced = referenced),
            (false, false) => t!("check.unstable_query.fix.blockers.referenced", referenced = referenced),
            (false, true) => t!("check.unstable_query.fix.blockers.table_lock"),
        });
        match (self.io_ms.is_some(), engine) {
            (true, Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite) => {}
            (false, Engine::Postgres) => summary.push(t!("check.unstable_query.fix.compare_storage")),
            (false, Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite) => {
                summary.push(t!("check.unstable_query.fix.compare_log_storage"))
            }
        }
        summary.push(t!("check.unstable_query.fix.inspect"));
        Fix { summary: summary.join(" "), sql, cost: None }
    }
}

/// How many rows an INSERT's VALUES list gives: its parenthesized groups, `None` without a VALUES list.
fn values_rows(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("values")?;
    let (mut depth, mut rows, mut quote) = (0i32, 0usize, false);
    for c in lower[at + "values".len()..].chars() {
        match c {
            '\'' => quote = !quote,
            _ if quote => {}
            '(' => {
                if depth == 0 {
                    rows += 1;
                }
                depth += 1;
            }
            ')' => depth -= 1,
            // Past the list: what it does on a conflict, or what it returns.
            _ if depth == 0 && !matches!(c, ',' | ' ' | '\n' | '\t' | '\r') => break,
            _ => {}
        }
    }
    (rows > 0).then_some(rows)
}

/// Whether the visible plan writes supplied values without a table/index read. Triggers, constraints and called
/// functions can do additional work that an estimated plan does not show; this narrows possibilities, not the cause.
fn writes_without_reading(plan: &Plan) -> bool {
    let nodes = plan.nodes();
    nodes.iter().any(|n| n.kind == NodeKind::Modify)
        && !nodes.iter().any(|n| matches!(n.kind, NodeKind::Scan | NodeKind::Seek | NodeKind::Lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Snapshot {
        let text =
            std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
        serde_json::from_str(&text).expect("snapshot")
    }

    /// The PostgreSQL demo with its query 3432126458605143571 made `text`, an insert of its values into
    /// order_items whose runs swing: 9,900 at 2 ms and 100 at 5 s. order_items gets a unique key on the order and
    /// the product, which the insert sets.
    fn unstable_insert(text: &str) -> Snapshot {
        let mut s = fixture("postgres-demo.json");
        let (runs, slow) = (10_000.0, 100.0);
        let mean = (2.0 * (runs - slow) + 5_000.0 * slow) / runs;
        let variance = ((2.0f64 - mean).powi(2) * (runs - slow) + (5_000.0f64 - mean).powi(2) * slow) / runs;
        let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "3432126458605143571").unwrap();
        q.text = text.into();
        (q.executions, q.mean_time_ms, q.total_time_ms) = (runs, mean, mean * runs);
        (q.stddev_time_ms, q.max_time_ms, q.rows) = (Some(variance.sqrt()), Some(5_000.0), Some(runs * 0.25));
        // A plan states its facts; these state nothing is known beyond the plan's shape.
        let (plan_facts, node_facts) = (
            serde_json::to_value(culprit_core::PlanFacts::default()).unwrap(),
            serde_json::to_value(culprit_core::PlanNodeFacts::default()).unwrap(),
        );
        q.plan = Some(
            serde_json::from_value(serde_json::json!({
                "engine": "postgres", "kind": "generic", "total_cost": 0.01, "warnings": [], "index_hints": [],
                "props": {}, "facts": plan_facts,
                "root": {
                    "id": 0, "op": "ModifyTable", "kind": "modify", "object": "public.order_items",
                    "detail": "Insert", "est_rows": 0.0, "est_executions": 1.0, "subtree_cost": 0.01,
                    "self_cost": 0.0, "warnings": [], "props": {"Conflict Resolution": "NOTHING"}, "facts": node_facts.clone(),
                    "children": [{
                        "id": 1, "op": "Result", "kind": "compute", "est_rows": 1.0, "est_executions": 1.0,
                        "subtree_cost": 0.01, "self_cost": 0.01, "warnings": [], "props": {}, "facts": node_facts, "children": []
                    }]
                }
            }))
            .expect("plan"),
        );
        let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
        s.workload.as_mut().unwrap().total_time_ms = total;
        let pkey = s.indexes.iter().find(|i| i.name == "order_items_pkey").cloned().expect("demo index");
        s.indexes.push(culprit_core::IndexStat {
            filter_facts: None,
            name: "order_items_order_product_key".into(),
            columns: vec!["order_id".into(), "product_id".into()],
            is_primary_key: false,
            ..pkey
        });
        s
    }

    fn unstable(s: Snapshot) -> Finding {
        crate::analyze(s)
            .findings
            .into_iter()
            .find(|f| f.id == "unstable-query:3432126458605143571")
            .expect("an unstable insert")
    }

    const SKIPS_EXISTING: &str =
        "INSERT INTO order_items (order_id, product_id, quantity) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING";

    // The aptabase reading: "Its plan reads no table (it writes the values it is given)" for an INSERT … ON CONFLICT
    // DO NOTHING that looks its key up in the primary key on every run, and finds it there 3 times in 4; and the
    // summary's "locks or deletes the referenced row" against the fix's "… or changes its key".
    #[test]
    fn an_insert_that_skips_existing_keys_says_what_each_run_reads() {
        let f = unstable(unstable_insert(SKIPS_EXISTING));
        assert!(
            f.summary.contains(
                "Its plan shows no read, but each run looks its key up in order_items_order_product_key to see whether a row already has it, and its foreign keys on product_id and order_id check each row it inserts against products and orders"
            ),
            "{}",
            f.summary
        );
        assert!(
            f.summary.contains(" 75% of its runs found their key already there and inserted nothing."),
            "{}",
            f.summary
        );
        assert!(!f.summary.contains("reads no table"), "{}", f.summary);
        // The same holders of the referenced row, in the same words, in the summary and the fix.
        let holders =
            "locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders";
        assert!(f.summary.contains(&format!("waiting for a transaction that {holders}")), "{}", f.summary);
        let fix = &f.fix.as_ref().unwrap().summary;
        assert!(fix.contains(&format!("and so does one that {holders}: keep those transactions short")), "{fix}");
        // A plain insert of its values: nothing skipped, nothing said about it.
        let f = unstable(unstable_insert(
            "INSERT INTO order_items (order_id, product_id, quantity) VALUES ($1, $2, $3), ($4, $5, $6) ON CONFLICT DO NOTHING",
        ));
        assert!(!f.summary.contains("inserted nothing"), "two rows a run: {}", f.summary);
    }

    // The aptabase reading: "Find what the slow runs wait for while they happen (Live…)" for slow runs that may number
    // 16 in 3.5 years. Too rare to catch live, the next ones are logged, the way settings are set on the server.
    #[test]
    fn slow_runs_too_rare_to_watch_are_logged_next_time() {
        // The demo's five hours: 100 slow runs, 20 an hour, are there to be watched.
        let f = unstable(unstable_insert(SKIPS_EXISTING));
        let fix = f.fix.as_ref().unwrap();
        assert!(fix.summary.starts_with("Find what the slow runs wait for while they happen (Live"), "{}", fix.summary);
        assert_eq!(fix.sql, None);
        // The same runs over three and a half years of counters.
        let rare = |platform: Option<culprit_core::Platform>| {
            let mut s = unstable_insert(SKIPS_EXISTING);
            s.workload.as_mut().unwrap().window_hours = 30_000.0;
            s.server.set_platform(platform);
            unstable(s).fix.unwrap()
        };
        let fix = rare(None);
        assert!(
            fix.summary.starts_with(
                "The extra time can come from as few as 100 slow runs in the 1250 days since statistics were reset, too few to count on catching one while it happens. Have the next ones logged instead. Set log_lock_waits to on and log_min_duration_statement to 2s"
            ),
            "{}",
            fix.summary
        );
        assert!(fix.summary.contains("with the process holding the lock"), "{}", fix.summary);
        let sql = fix.sql.unwrap_or_default();
        assert!(sql.contains("ALTER SYSTEM SET log_lock_waits") && sql.contains("pg_reload_conf()"), "{sql}");
        // A managed server sets them in its own control.
        let fix = rare(Some(culprit_core::Platform::AmazonRds));
        assert!(fix.summary.contains("DB parameter group"), "{}", fix.summary);
        assert_eq!(fix.sql, None);
    }

    // The aptabase-us reading: "watch them (Live shows each session's wait…)" for the statement that replaced a stopped
    // insert, whose slowest run so far took 88 ms: there is nothing to catch live, and its next slow runs are logged,
    // the way unstable-query logs any too rare to watch.
    #[test]
    fn historical_outliers_do_not_change_logging_for_a_fast_replacement() {
        let mut s = unstable_insert(SKIPS_EXISTING);
        let w = s.workload.as_mut().unwrap();
        w.window_hours = 48.0;
        let q = w.queries.iter_mut().find(|q| q.id == "3432126458605143571").unwrap();
        (q.first_seen, q.last_seen) = (Some("2026-10-09T12:00:00Z".into()), Some("2026-10-10T08:00:00Z".into()));
        let next = w.queries.iter_mut().find(|q| q.id == "2885972271926804069").unwrap();
        next.text = "INSERT INTO order_items (order_id, product_id, quantity, note) VALUES ($1, $2, $3, $4)".into();
        (next.first_seen, next.last_seen) = (Some("2026-10-10T08:05:00Z".into()), Some("2026-10-11T11:50:00Z".into()));
        (next.executions, next.mean_time_ms, next.total_time_ms, next.max_time_ms) =
            (5_000.0, 0.5, 2_500.0, Some(88.0));
        next.plan = None;
        let f = unstable(s);
        assert!(f.inactive, "it stopped");
        let fix = f.fix.unwrap();
        assert!(fix.summary.contains("This statement no longer runs."), "{}", fix.summary);
        assert!(fix.summary.contains("2885972271926804069") && fix.summary.contains("88 ms"));
        assert!(fix.summary.contains("do not establish a current problem"));
        assert!(!fix.summary.contains("Set log_") && !fix.summary.contains("Have the next ones logged"));
        assert_eq!(fix.sql, None, "a finding on a statement that stopped runs nothing for it");
    }

    fn unstable_read() -> Snapshot {
        let mut s = crate::waits::snapshot_with(Engine::Mssql);
        let w = s.workload.as_mut().unwrap();
        let q = w.queries.iter_mut().find(|q| q.id == "10").expect("the demo's read of support_tickets");
        let (runs, slow) = (8_000.0, 10.0);
        let mean = (1.2 * (runs - slow) + 1_100.0 * slow) / runs;
        let variance = ((1.2f64 - mean).powi(2) * (runs - slow) + (1_100.0f64 - mean).powi(2) * slow) / runs;
        (q.executions, q.mean_time_ms, q.total_time_ms) = (runs, mean, mean * runs);
        (q.stddev_time_ms, q.max_time_ms) = (Some(variance.sqrt()), Some(1_100.0));
        q.plans.truncate(1);
        q.waits.clear();
        q.waits_coverage = None;
        q.spread = None;
        w.total_time_ms = w.queries.iter().map(|q| q.total_time_ms).sum();
        s
    }

    #[test]
    fn qualified_aggregates_describe_waits_without_assigning_them_to_the_slow_calls() {
        let mut s = unstable_read();
        let w = s.workload.as_mut().unwrap();
        let q = w.queries.iter_mut().find(|q| q.id == "10").unwrap();
        q.waits_coverage = Some(culprit_core::QueryWaitCoverage {
            classes: vec![
                culprit_core::telemetry::WaitClass::Lock,
                culprit_core::telemetry::WaitClass::Network,
                culprit_core::telemetry::WaitClass::Memory,
                culprit_core::telemetry::WaitClass::Cpu,
            ],
            complete: true,
        });
        let wait = |category: &str, wait_type: &str, ms: f64| culprit_core::WaitStat {
            category: category.into(),
            wait_type: Some(wait_type.into()),
            wait_ms: ms,
            tasks: None,
        };
        q.waits =
            vec![wait("Network", "Network IO", 3_406.0), wait("Memory", "Memory", 43.0), wait("CPU", "CPU", 11.0)];
        let total: f64 = w.queries.iter().map(|q| q.total_time_ms).sum();
        w.total_time_ms = total;
        let found =
            |s: Snapshot| unstable_query(&s).into_iter().find(|f| f.id == "unstable-query:10").expect("unstable");
        let f = found(s.clone());
        assert!(
            f.summary.contains("Query Store recorded no lock waits in this qualified observation")
                && f.summary.contains("Network IO (3.4 s)"),
            "{}",
            f.summary
        );
        assert!(!f.summary.contains("Check the waits"), "{}", f.summary);
        assert!(f.summary.contains("These totals do not identify which calls were slow."), "{}", f.summary);
        // Aggregate waits keep blocking and parameter sensitivity open even when the class is qualified.
        let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "10").unwrap();
        q.waits = vec![wait("Lock", "Lock", 10_000.0)];
        let f = found(s);
        assert!(f.summary.contains("Query Store recorded 10.0 s of lock waits"), "{}", f.summary);
        assert!(
            f.summary.contains("do not identify which calls waited or exclude parameter-sensitive plans"),
            "{}",
            f.summary
        );
        let fix = f.fix.unwrap();
        assert!(fix.summary.contains("Pick the lightest option"), "{}", fix.summary);
        assert!(fix.summary.contains("Inspect the blocking transaction"), "{}", fix.summary);
    }

    #[test]
    fn incomplete_undeclared_and_malformed_wait_coverage_never_rule_out_locks() {
        use culprit_core::{QueryWaitCoverage, telemetry::WaitClass};
        for coverage in [
            None,
            Some(QueryWaitCoverage { classes: vec![WaitClass::Io], complete: true }),
            Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: false }),
            Some(QueryWaitCoverage { classes: vec![WaitClass::Lock, WaitClass::Lock], complete: true }),
        ] {
            let mut snapshot = unstable_read();
            let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| query.id == "10").unwrap();
            query.waits_coverage = coverage;
            query.waits = vec![culprit_core::WaitStat {
                category: "IO".into(),
                wait_type: Some("native read wait".into()),
                wait_ms: 1000.0,
                tasks: None,
            }];
            let case = unstable_queries(&snapshot).into_iter().find(|case| case.query.id == "10").unwrap();
            assert!(matches!(&case.recorded, Recorded::Incomplete));
            let finding = unstable_query_finding(&snapshot, case);
            assert!(finding.summary.contains("Wait coverage is incomplete"), "{}", finding.summary);
            assert!(!finding.summary.contains("recorded no lock waits"), "{}", finding.summary);
            assert!(!finding.summary.contains("locks weren't"), "{}", finding.summary);
        }
    }

    #[test]
    fn positive_partial_waits_keep_both_investigations_and_source_wording_follows_explicit_kind() {
        use culprit_core::{QueryWaitCoverage, telemetry::WaitClass};
        for coverage in [
            None,
            Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: false }),
            Some(QueryWaitCoverage { classes: vec![WaitClass::Io], complete: true }),
        ] {
            let mut snapshot = unstable_read();
            let workload = snapshot.workload.as_mut().unwrap();
            workload.kind = None;
            let query = workload.queries.iter_mut().find(|query| query.id == "10").unwrap();
            query.waits_coverage = coverage;
            query.waits = vec![culprit_core::WaitStat {
                category: "Lock".into(),
                wait_type: None,
                wait_ms: 10_000.0,
                tasks: None,
            }];
            let case = unstable_queries(&snapshot).into_iter().find(|case| case.query.id == "10").unwrap();
            assert!(matches!(&case.recorded, Recorded::ObservedLocks { lock_ms, .. } if *lock_ms == 10_000.0));
            let finding = unstable_query_finding(&snapshot, case);
            assert!(
                finding.summary.contains("At least 10.0 s of lock waits were recorded by SQL Server"),
                "{}",
                finding.summary
            );
            assert!(!finding.summary.contains("recorded by Query Store"), "{}", finding.summary);
            assert!(
                finding.summary.contains("do not identify which calls waited or rule out parameter-sensitive plans"),
                "{}",
                finding.summary
            );
            let fix = finding.fix.unwrap();
            assert!(fix.summary.contains("Inspect the blocking transaction"), "{}", fix.summary);
            assert!(fix.summary.contains("Pick the lightest option"), "{}", fix.summary);
        }
    }

    #[test]
    fn a_values_list_counts_its_rows() {
        assert_eq!(values_rows("INSERT INTO t (a, b) VALUES ($1, $2) ON CONFLICT DO NOTHING"), Some(1));
        assert_eq!(values_rows("INSERT INTO t (a, b) VALUES ($1, '(x)'), ($3, $4)"), Some(2));
        assert_eq!(values_rows("INSERT INTO t (a) SELECT a FROM u"), None);
    }

    #[test]
    fn verification_uses_new_runs_from_the_sources_own_periods() {
        for engine in ["postgres", "mysql"] {
            let s = fixture(&format!("{engine}-demo.json"));
            let step = verify_new_runs(&s);
            assert!(step.contains("keeps running totals") && step.contains("use the new log samples"), "{step}");
            assert!(step.contains("same logging threshold"), "{step}");
        }
        let mut s = fixture("mssql-demo.json");
        let step = verify_new_runs(&s);
        assert!(step.contains("equal Query Store windows") && step.contains("only runs after it"), "{step}");
        assert!(step.contains("lock waits per run where recorded"), "{step}");
        let workload = s.workload.as_mut().unwrap();
        workload.kind = Some(culprit_core::WorkloadKind::PlanCache);
        workload.source = "Plan cache".into();
        workload.windowed = false;
        let step = verify_new_runs(&s);
        assert!(step.contains("actual execution plans") && step.contains("Plan cache keeps running totals"), "{step}");
        let s = fixture("sqlite-demo.json");
        let step = verify_new_runs(&s);
        assert!(step.contains("equal-length query-log samples") && step.contains("timestamps"), "{step}");
        assert!(!step.contains("server log") && !step.contains("keeps running totals"), "{step}");
        let mut s = fixture("mysql-demo.json");
        s.workload.as_mut().unwrap().kind = Some(culprit_core::WorkloadKind::SlowLog);
        assert!(verify_new_runs(&s).contains("equal-length query-log samples"));
    }
}
