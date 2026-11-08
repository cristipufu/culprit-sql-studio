use super::*;
use culprit_core::insights::PlanInsight;

/// The advice a heavy statement's evidence permits: chosen with the rest of its finding ([`decide`]), worded by
/// [`Remedy::fix`].
pub(super) enum Remedy<'a> {
    /// What to do about where its idle time goes ([`IdleKind::fix`]).
    Idle(IdleKind),
    /// Measure before changing anything: an actual plan's row counts, and the IO on the table its lookups read.
    Measure { schema: String, table: String },
    /// A scan step's own fix, by the plan's rules for it ([`scan_step`]).
    ScanStep(ScanStep<'a>),
    /// The settings of the driver or the connection pool that sends it ([`session_fix`]).
    Session,
    /// The plan's own key-lookups advice for the hot step, quoted.
    KeyLookups(PlanInsight),
    /// An index whose key order matches the sort that is this part of the work.
    BigSort(Part),
    /// Storage waiting is observed, its cause (cache pressure, cold pages or latency) not established.
    StorageWait { cheap_plan: bool, finds_nothing: bool },
    /// Fewer runs: for runs measured cheap (`cheap_plan`), or, with no plan to judge them by, for their count alone.
    FewerRuns { cheap_plan: bool, lever: RunsLever },
    /// chatty-query's advice for a statement that hands out numbers, quoted.
    Counter(String),
    /// What to check of its kind of statement, where nothing more specific applies.
    PerKind(KindAdvice),
    /// Optimizer statistics maintenance: its schedule, reviewed only with evidence of its cost.
    StatisticsMaintenance,
    /// A rewrite to evaluate for a filter applied late, quoted from the observation Findings, Plans and the advisor
    /// share ([`late_filter_rewrite`]).
    Rewrite(PlanInsight),
}

/// How much of the work the hot step is, from the share of the plan's cost on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    /// Half of what the plan costs, or more.
    Most,
    /// Less than half, yet the largest part.
    Largest,
    /// A plan that can state no share: the step it costs highest.
    CostedHighest,
}

/// How a statement whose cost is how often it runs runs less.
pub(super) enum RunsLever {
    /// A write's levers ([`write_lever`]).
    Write(WriteLever),
    /// A read whose runs mostly find nothing: let the write do the check, or poll less often.
    FindsNothing,
    /// A result cached, calls batched, or a call skipped when its answer is already known.
    Cache,
}

/// The levers for a write whose runs are cheap and many ([`write_lever`]).
pub(super) struct WriteLever {
    how: WriteHow,
    /// Indexes on its table no read uses, which every write still keeps up to date.
    unused: Vec<String>,
}

#[derive(Clone, Copy)]
enum WriteHow {
    /// A get-or-create: skip the call where the application has the row, or don't rewrite the row that is there.
    GetOrCreate,
    /// An upsert that updates the row it finds: only where its values differ.
    UpsertUpdate,
    /// Skip the runs that change nothing: at least this share of them, or exactly.
    SkipNothing { share: f64, exact: bool },
    /// More rows per statement, in one transaction unless another transaction can insert the same key.
    Batch { one_transaction: bool },
}

/// What to check of a statement, by its kind, where nothing more specific applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KindAdvice {
    /// That it finds the rows it changes through an index, and changes no more than it needs.
    Write,
    /// That it locks only the rows its transaction changes, and the transaction commits soon after.
    Locking,
    /// That it aggregates no more rows than it needs; and, for one that folds many rows into each it returns, whether
    /// its result could be kept up to date.
    Aggregate { keep_up_to_date: bool },
    /// That it reads no more rows or columns than it needs.
    Read,
}

/// The advice for a heavy statement no single step's own measurement explains, with what it moves: fewer runs (the
/// pool's settings, a cache, a skipped or batched call), or cheaper runs. The verify step expects what it moves
/// ([`verify_line`]).
pub(super) fn heavy_remedy<'a>(
    snapshot: &Snapshot,
    hot: Option<(&'a Plan, &'a PlanNode)>,
    per_day_runs: Option<f64>,
    q: &QueryStat,
    kind: StatementKind,
    cheap_plan: bool,
) -> (Remedy<'a>, Shows) {
    // The driver or the pool sends these around the application's statements: its settings are the lever.
    if culprit_core::sql::is_session_statement(&q.text) {
        return (Remedy::Session, Shows::FewerRuns);
    }
    if let Some((plan, node)) = hot {
        if let Some(step) = scan_step(snapshot, plan, node, q, kind) {
            return (Remedy::ScanStep(step), Shows::CheaperRuns);
        }
        // A covering index or a sorted index only where the plan's own rule says the step is worth it: covering a
        // seek that finds a row or two copies the selected columns into the index for nothing.
        if culprit_core::insights::has_insight(plan, "key-lookups", node)
            && let Some(i) = crate::views::plan_advice::catalog_insights(snapshot, q, plan)
                .into_iter()
                .find(|i| i.rule == "key-lookups" && i.nodes.contains(&node.id))
        {
            return (Remedy::KeyLookups(i), Shows::CheaperRuns);
        }
        if culprit_core::insights::has_insight(plan, "big-sort", node) {
            // "Most of the work" only from half of what the plan costs, and a share only where the plan can state one.
            let part = match plan.step_share(node) {
                Some(share) if share >= 0.5 => Part::Most,
                Some(_) => Part::Largest,
                None => Part::CostedHighest,
            };
            return (Remedy::BigSort(part), Shows::CheaperRuns);
        }
    }
    // Storage waiting is observed; whether it comes from cache pressure, cold pages or latency is not established.
    if matches!(kind, StatementKind::Read | StatementKind::Aggregate)
        && dominant_wait(q).is_some_and(|w| w.on == WaitOn::Storage)
    {
        let finds_nothing = finds_nothing_mostly(q, kind);
        return (Remedy::StorageWait { cheap_plan, finds_nothing }, Shows::CheaperRuns);
    }
    // How often it runs is the cost on the same condition the summary says so: runs measured cheap. Only a statement
    // with no plan to judge its runs by is judged by its count alone.
    if cheap_plan {
        return (fewer_runs(snapshot, q, kind, true), Shows::FewerRuns);
    }
    if q.plan.as_ref().is_none_or(|p| !p.weighed())
        && per_day_runs.is_some_and(|r| r >= 100_000.0)
        && snapshot.workload.as_ref().map_or(q.mean_time_ms, |w| w.statement_mean_ms(q)) < 5.0
    {
        return (fewer_runs(snapshot, q, kind, false), Shows::FewerRuns);
    }
    let remedy = match kind {
        StatementKind::Write => match super::super::chatty_query::counter_advice(snapshot, q) {
            Some(advice) => Remedy::Counter(advice),
            None => Remedy::PerKind(KindAdvice::Write),
        },
        StatementKind::Locking => Remedy::PerKind(KindAdvice::Locking),
        // A result worth keeping up to date is one that folds many rows into each it returns, not a few-row lookup
        // that groups.
        StatementKind::Aggregate => Remedy::PerKind(KindAdvice::Aggregate { keep_up_to_date: folds_many_rows(q) }),
        _ => Remedy::PerKind(KindAdvice::Read),
    };
    (remedy, Shows::CheaperRuns)
}

impl Remedy<'_> {
    /// The fix, in words, and SQL where the remedy quotes some.
    pub(super) fn fix(&self, snapshot: &Snapshot, q: &QueryStat) -> Fix {
        match self {
            Remedy::Idle(kind) => kind.fix(snapshot, q),
            Remedy::Measure { schema, table } => measure_fix(snapshot, schema, table),
            Remedy::ScanStep(step) => step.fix(snapshot),
            Remedy::Session => advice(session_fix(snapshot.server.engine, &q.text)),
            Remedy::KeyLookups(i) => Fix { summary: i.fix.clone(), sql: i.fix_sql.clone(), cost: None },
            Remedy::BigSort(part) => {
                let most = match part {
                    Part::Most => "Most of the work",
                    Part::Largest => "The largest part of the work",
                    Part::CostedHighest => "The step the plan costs highest",
                };
                advice(format!(
                    "{most} is sorting. An index whose key order matches the sort lets the database read rows already sorted."
                ))
            }
            Remedy::StorageWait { cheap_plan, finds_nothing } => {
                advice(storage_fix(snapshot, q, *cheap_plan, *finds_nothing))
            }
            Remedy::FewerRuns { cheap_plan, lever } => advice(fewer_runs_fix(snapshot, q, *cheap_plan, lever)),
            Remedy::Counter(advice_text) => advice(advice_text.clone()),
            Remedy::PerKind(kind) => advice(match kind {
                KindAdvice::Write => "Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).".into(),
                KindAdvice::Locking => "Find this statement in the application: it locks the rows it finds until its transaction ends. Check that it locks only the rows the transaction goes on to change, and that the transaction commits soon after.".into(),
                KindAdvice::Aggregate { keep_up_to_date: true } => "Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.".into(),
                KindAdvice::Aggregate { keep_up_to_date: false } => "Find this statement in the application and check whether it aggregates more rows than it needs.".into(),
                KindAdvice::Read => "Find this statement in the application and check whether it reads more rows or columns than it needs.".into(),
            }),
            Remedy::StatisticsMaintenance => advice("Check whether this was an automatic refresh or scheduled maintenance. Keep necessary statistics current; review an unnecessarily frequent schedule or full scan only with evidence of its cost.".into()),
            Remedy::Rewrite(rewrite) => advice(rewrite.fix.clone()),
        }
    }
}

/// Whether an aggregate's runs read many pages (rows examined, MySQL) for each row they return.
fn folds_many_rows(q: &QueryStat) -> bool {
    match (q.reads_per_execution(), q.rows_per_execution()) {
        (Some(reads), Some(rows)) => reads / rows.max(1.0) >= MANY_READS_PER_ROW,
        _ => false,
    }
}

/// What the plan's own rules say of a step that scans a table with a filter ([`scan_step`]).
#[derive(Clone, Copy)]
pub(super) enum ScanStep<'a> {
    /// The scan keeps most of its table, yet runs return an order of magnitude fewer rows: a condition later in the
    /// plan narrows them, and the advice is about that condition ([`narrowed_later_fix`]).
    NarrowedLater { plan: &'a Plan, table: &'a str, kept: f64, returned: f64, narrowing: Narrowing<'a> },
    /// The scan keeps too much for an index, about what the runs return: read less.
    ReadLess { table: &'a str, kept: f64 },
    /// The filter keeps few rows where the filtered-scan check would suggest an index.
    Index { table: &'a str },
}

/// The fix for a step that scans a table with a filter, by the plan's own rules for it: the statement needs about what
/// the scan keeps (read less), a condition later in the plan narrows it (that condition's index), or the filter keeps
/// few rows where the filtered-scan check would suggest an index. None when the plan says none of these.
pub(super) fn scan_step<'a>(
    snapshot: &Snapshot,
    plan: &'a Plan,
    node: &'a PlanNode,
    q: &QueryStat,
    kind: StatementKind,
) -> Option<ScanStep<'a>> {
    if node.kind != NodeKind::Scan || node.predicate.is_none() {
        return None;
    }
    let table = node.object.as_deref().unwrap_or("the table");
    // An index only where the filtered-scan check would suggest one: enough rows, a filter that keeps few by an
    // estimate that isn't a guess, columns an index can seek on, and no index already leading with them.
    let index_pays = scan_groups(snapshot)
        .iter()
        .any(|g| g.queries.iter().any(|(scanned, id)| scanned.id == q.id && *id == node.id));
    match node.kept_share() {
        // The bar every index suggestion uses: past it the scan is the right plan, and the fix is to read less, once
        // the rows the statement returns say it needs what the scan keeps.
        Some(kept) if kept > culprit_core::INDEX_KEEPS_AT_MOST => {
            let kept_rows = node.rows_read_total().map(|read| read * kept);
            // The nearest condition above the scan: where the plan narrows what the scan keeps.
            let narrowing = narrowing_above(plan, node.id);
            match (kept_rows, q.rows_handled_per_execution()) {
                // Runs return an order of magnitude fewer rows than the scan keeps: they're narrowed later. An
                // aggregate returns one row per group whatever it reads, so its rows say so only where a condition
                // above the scan narrows them before they're grouped.
                (Some(k), Some(returned))
                    if returned * culprit_core::MISESTIMATE_FACTOR < k
                        && (kind != StatementKind::Aggregate || !matches!(narrowing, Narrowing::Unseen)) =>
                {
                    Some(ScanStep::NarrowedLater { plan, table, kept, returned, narrowing })
                }
                // About as many as the scan keeps: what the caller asks for. Without the rows the runs return,
                // nothing says so, and nothing is claimed.
                (Some(_), Some(_)) => Some(ScanStep::ReadLess { table, kept }),
                _ => None,
            }
        }
        _ if index_pays => Some(ScanStep::Index { table }),
        _ => None,
    }
}

impl ScanStep<'_> {
    pub(super) fn fix(&self, snapshot: &Snapshot) -> Fix {
        match *self {
            ScanStep::NarrowedLater { plan, table, kept, returned, narrowing } => {
                narrowed_later_fix(snapshot, plan, table, kept, returned, narrowing)
            }
            ScanStep::ReadLess { table, kept } => advice(format!(
                "The plan reads all of {table} and keeps about {} of the rows, too many for an index to help. Read only the rows and columns the caller needs, or page through them.",
                format::percent(kept)
            )),
            ScanStep::Index { table } => advice(format!(
                "The plan scans {table} and filters afterwards. An index on the filtered columns turns that into a seek; open the plan to see the predicate."
            )),
        }
    }
}

/// Fewer runs, for a statement whose runs are fast and whose cost is how many there are. A read's answer can be cached,
/// or skipped when it's known, unless its runs mostly find nothing: a check whether a row exists, whose answer goes
/// stale the moment one is written. A write has to happen, so its levers are skipping it where it changes nothing,
/// more rows per statement, and an index no read uses ([`write_lever`]); or, for one that hands out numbers,
/// chatty-query's.
fn fewer_runs<'a>(snapshot: &Snapshot, q: &QueryStat, kind: StatementKind, cheap_plan: bool) -> Remedy<'a> {
    let lever = match kind {
        StatementKind::Write => match super::super::chatty_query::counter_advice(snapshot, q) {
            Some(advice) => return Remedy::Counter(advice),
            None => RunsLever::Write(write_lever(snapshot, q)),
        },
        _ if finds_nothing_mostly(q, kind) => RunsLever::FindsNothing,
        _ => RunsLever::Cache,
    };
    Remedy::FewerRuns { cheap_plan, lever }
}

/// [`fewer_runs`] in words, opened by what shows the cost is how often it runs: a plan measured cheap, or the count.
fn fewer_runs_fix(snapshot: &Snapshot, q: &QueryStat, cheap_plan: bool, lever: &RunsLever) -> String {
    let opening = if cheap_plan {
        "Each run is fast and its plan is already cheap; the cost is in how often it runs."
    } else {
        "Each run is fast; the cost is in how often it runs."
    };
    match lever {
        RunsLever::Write(lever) => write_fix(snapshot, q, cheap_plan, lever),
        RunsLever::FindsNothing => format!("{opening} {}", finds_nothing(snapshot, q)),
        RunsLever::Cache => format!(
            "{opening} Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known."
        ),
    }
}

/// What a write does to rows, in sentences: ("inserts", "insert") for an INSERT.
fn write_verbs(q: &QueryStat) -> (&'static str, &'static str) {
    match first_verb(main_statement(&q.text)).map(|(_, v)| v) {
        // Its rows are the ones it inserted and the ones it updated on a conflict.
        Some("insert") if upsert(q) != Upsert::No => ("inserts or updates", "insert or update"),
        Some("insert") => ("inserts", "insert"),
        Some("update") => ("updates", "update"),
        Some("delete") => ("deletes", "delete"),
        _ => ("changes", "change"),
    }
}

/// What an INSERT does with a row whose key is already there, where it updates it: PostgreSQL's `ON CONFLICT … DO
/// UPDATE`. Every such run writes a new version of the row (and its WAL), even one that sets a column to the value it
/// has, which a get-or-create does only so that RETURNING gives back the row there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Upsert {
    /// It inserts, or skips the row (`DO NOTHING`).
    No,
    /// `DO UPDATE` with new values (`SET c = EXCLUDED.c`).
    Update,
    /// `DO UPDATE` that sets each column to itself (`SET salt = app_salts.salt`): the row is rewritten unchanged.
    GetOrCreate,
}

fn upsert(q: &QueryStat) -> Upsert {
    let text = squeeze(main_statement(&q.text));
    let Some(at) = text.find(" do update set ").filter(|_| text.contains(" on conflict")) else { return Upsert::No };
    let rest = &text[at + " do update set ".len()..];
    let end = [" where ", " returning "].iter().filter_map(|k| rest.find(k)).min().unwrap_or(rest.len());
    let leaf = |side: &str| side.trim().rsplit('.').next().unwrap_or("").to_string();
    let to_itself = rest[..end].split(',').all(|assignment| {
        assignment.split_once('=').is_some_and(|(column, value)| {
            let value = value.trim();
            !value.starts_with("excluded.") && !leaf(column).is_empty() && leaf(column) == leaf(value)
        })
    });
    if to_itself { Upsert::GetOrCreate } else { Upsert::Update }
}

/// Whether an INSERT writes one row of values a run (`INSERT … VALUES (…)`, one list): then the runs that change a row
/// are exactly as many as the rows it changed.
fn one_row_a_run(q: &QueryStat) -> bool {
    let text = squeeze(main_statement(&q.text));
    text.starts_with("insert") && text.contains(" values(") && !text.contains("),(") && !text.contains(" select ")
}

/// The share of a write's runs that change nothing, from the rows they change on average: at least 1 − rows a run (a
/// run that changes something changes a row or more), exactly that where each run writes one row of values; with
/// whether it is exact. `None` for runs that change half a row or more on average.
fn runs_changing_nothing(q: &QueryStat) -> Option<(f64, bool)> {
    let rows = q.rows_handled_per_execution().filter(|r| *r < 0.5)?;
    Some((1.0 - rows, one_row_a_run(q)))
}

/// What a write's runs cost, from its own counters, for a plan with nothing to make faster: the rows a run changes
/// (and how many runs change none), and the log it writes where the engine counts it. "Each run inserts 0.25 rows on
/// average (75% of its runs insert nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.7 KB
/// and 2.7 full-page images (…) for each row it inserts."
pub(super) fn write_cost(snapshot: &Snapshot, q: &QueryStat) -> String {
    // A statement that stopped running is told in the past.
    let stopped = crate::all_stopped(snapshot, &[q]);
    let (verb, verb_base) = match write_verbs(q) {
        (verb, base) if stopped => (past_tense(verb), past_tense(base)),
        (verb, base) => (verb.to_string(), base.to_string()),
    };
    let mut out = if upsert(q) == Upsert::No && runs_changing_nothing(q).is_some() {
        if stopped {
            "Its plan had nothing to make faster: checking for rows still cost work when a run changed nothing."
                .to_string()
        } else {
            "Its plan has nothing to make faster: checking for rows still costs work when a run changes nothing."
                .to_string()
        }
    } else if stopped {
        "Its plan had nothing to make faster: what a run cost was the write itself (its log, its index entries, the checks of its keys).".to_string()
    } else {
        "Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys).".to_string()
    };
    let rows = q.rows_handled_per_execution();
    let postgres = snapshot.server.engine == Engine::Postgres;
    let log_word = if postgres { "WAL" } else { "transaction log" };
    let costs = q.costs.as_ref();
    let log = costs.and_then(|c| c.log_bytes).filter(|b| *b > 0.0 && q.executions > 0.0);
    match rows {
        Some(rows) => {
            let n = rows_a_run(rows);
            // Culprit's scans keep a statement's runs and time, not its rows: where its summary leads with them, what a
            // run changes is said over the totals it comes from (a recent entry's are mostly those scans' runs).
            let over = crate::support::totals_basis(snapshot, q).map(|b| format!(" over {b}")).unwrap_or_default();
            out.push_str(&format!(" Each run {verb} {n} row{} on average{over}", if n == "1" { "" } else { "s" }));
            if upsert(q) == Upsert::GetOrCreate {
                out.push_str(
                    " (a run that finds the row there rewrites it: its DO UPDATE sets a column to the value it has, which still writes a new version of the row)",
                );
            }
            if let Some((share, exact)) = runs_changing_nothing(q) {
                out.push_str(&format!(
                    " ({}{} of its runs {verb_base} nothing)",
                    if exact { "" } else { "at least " },
                    format::percent(share)
                ));
            }
        }
        None if log.is_some() => out.push_str(" Each run"),
        None => return out,
    }
    let Some(log) = log else {
        out.push('.');
        return out;
    };
    let share = costs
        .and_then(|c| c.log_share)
        .filter(|s| *s >= 0.5)
        .map(|s| format!(", {} of all the {log_word} the statements wrote", format::percent(s)))
        .unwrap_or_default();
    out.push_str(&format!(
        "{} {} {} of {log_word}{share}",
        if rows.is_some() { " and" } else { "" },
        if stopped { "wrote" } else { "writes" },
        format::bytes(log / q.executions)
    ));
    let images = costs.and_then(|c| c.log_full_pages).filter(|n| *n > 0.0 && postgres);
    const IMAGE: &str = "full-page images (a page logged whole the first time it changes after a checkpoint)";
    // Per row it changes, where that differs from per run.
    let per_row = rows.filter(|r| format::count(*r) != "1").map(|r| r * q.executions).filter(|r| *r >= 1.0);
    match (per_row, images) {
        (Some(changed), Some(images)) => out.push_str(&format!(
            ": {} and {} {IMAGE} for each row it {verb}.",
            format::bytes(log / changed),
            format::count(images / changed)
        )),
        (Some(changed), None) => out.push_str(&format!(": {} for each row it {verb}.", format::bytes(log / changed))),
        (None, Some(images)) => out.push_str(&format!(", with {} {IMAGE}.", format::count(images / q.executions))),
        (None, None) => out.push('.'),
    }
    out
}

/// A write's verbs ([`write_verbs`]) in the past, for a statement that stopped running: "inserts or updates" →
/// "inserted or updated".
fn past_tense(verbs: &str) -> String {
    verbs
        .split(' ')
        .map(|w| match w.trim_end_matches('s') {
            "insert" => "inserted",
            "update" => "updated",
            "delete" => "deleted",
            "change" => "changed",
            _ => w,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rows a run, as a count reads them, and to two decimals under one: 0.25 rows a run, not "0.3", beside "75% of its
/// runs".
pub(super) fn rows_a_run(rows: f64) -> String {
    if rows > 0.0 && rows < 1.0 {
        let text = format!("{rows:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        format::count(rows)
    }
}

/// The levers for a write whose runs are cheap and many. Where most runs change nothing (an insert of a row that is
/// already there), skipping them; otherwise more rows per statement, in one transaction unless another transaction can
/// insert the same key (holding it longer makes that one wait longer); and dropping an index only where the table has
/// one no read uses.
fn write_lever(snapshot: &Snapshot, q: &QueryStat) -> WriteLever {
    let (_, verb) = write_verbs(q);
    let table = write_targets(q, snapshot.server.engine)
        .into_iter()
        .find_map(|o| snapshot.tables.iter().find(|t| names_table(&o, &t.schema, &t.name)));
    let how = match (upsert(q), runs_changing_nothing(q)) {
        // A get-or-create rewrites the row it finds on every run that finds one: the levers are not sending it when
        // the application has the row, and not rewriting it when it is there.
        (Upsert::GetOrCreate, _) => WriteHow::GetOrCreate,
        (Upsert::Update, _) => WriteHow::UpsertUpdate,
        (Upsert::No, Some((share, exact))) => WriteHow::SkipNothing { share, exact },
        // Several rows in one transaction hold their locks until it commits: where another transaction can insert
        // the same key, it waits that much longer.
        (Upsert::No, None) => WriteHow::Batch {
            one_transaction: !table.is_some_and(|t| verb == "insert" && inserts_repeatable_key(snapshot, q, t)),
        },
    };
    // An index no read uses still costs every write an entry: the one index advice a write's count supports.
    let unused: Vec<String> = table
        .map(|t| {
            snapshot
                .indexes_on(&t.schema, &t.name)
                .filter(|i| crate::indexes::is_unused(snapshot, i))
                .map(|i| i.name.clone())
                .collect()
        })
        .unwrap_or_default();
    WriteLever { how, unused }
}

/// [`write_lever`] in words. That its plan has nothing to make faster only where a plan measured its runs cheap
/// (`cheap`); a statement judged by its count alone has none.
fn write_fix(snapshot: &Snapshot, q: &QueryStat, cheap: bool, lever: &WriteLever) -> String {
    let engine = snapshot.server.engine;
    let (_, verb) = write_verbs(q);
    // A statement that stopped running is told in the past.
    let nothing = matches!(lever.how, WriteHow::SkipNothing { .. });
    let opening = match (crate::all_stopped(snapshot, &[q]), cheap, nothing) {
        (true, _, true) => {
            "Each run was fast, but most of its runs changed nothing: the cost was in sending them anyway."
        }
        (false, _, true) => {
            "Each run is fast, but most of its runs change nothing: the cost is in sending them anyway."
        }
        (true, true, false) => {
            "Each run was fast and its plan had nothing to make faster: what cost was the write itself, every time it ran."
        }
        (true, false, false) => "Each run was fast; what cost was the write itself, every time it ran.",
        (false, true, false) => {
            "Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs."
        }
        (false, false, false) => "Each run is fast; what costs is the write itself, every time it runs.",
    };
    let how = match lever.how {
        WriteHow::GetOrCreate => "It is a get-or-create: its DO UPDATE sets a column to the value it has only so that RETURNING gives back the row already there, and that still writes a new version of the row (and its WAL) on every run that finds one. Skip the call where the application already has the row (it read or created it earlier and keeps it), or evaluate ON CONFLICT DO NOTHING RETURNING followed by a separate SELECT when no row is returned. That is not an equivalent atomic replacement: account for the transaction isolation level, concurrent deletion, a missing row and serialization failures with appropriate locking or retries. Preserve any required UPDATE-trigger effects; keep the atomic upsert if the application depends on them.".to_string(),
        WriteHow::UpsertUpdate => "On a conflict it updates the row it finds, writing a new version of it even when the values are the same: give its DO UPDATE a WHERE that updates only a row whose values differ (`WHERE t.c IS DISTINCT FROM EXCLUDED.c`), and skip the call where the application knows nothing changed.".to_string(),
        WriteHow::SkipNothing { share, exact } => {
            let skip = if verb == "insert" {
                "skip the statement where the application already knows the row exists (it read the row, or wrote it earlier, and keeps the value for as long as it holds), and send it only when the row may be missing.".to_string()
            } else {
                format!(
                    "skip the statement where the application already knows there is nothing to {verb} (the rows aren't there, or already hold the values)."
                )
            };
            // What its runs did, in the past once it stopped; the advice stays for the code that sends it.
            let did = if crate::all_stopped(snapshot, &[q]) { past_tense(verb) } else { verb.to_string() };
            format!(
                "{}{} of its runs {did} nothing{}: {skip}",
                if exact { "" } else { "At least " },
                format::percent(share),
                crate::support::totals_basis(snapshot, q).map(|basis| format!(" over {basis}")).unwrap_or_default()
            )
        }
        WriteHow::Batch { one_transaction } => {
            let many = match verb {
                "update" | "delete" => many_keys(engine, &verb.to_ascii_uppercase()),
                _ => bulk_insert(engine).to_string(),
            };
            let transaction = if one_transaction { ", in one transaction" } else { "" };
            format!(
                "{} several rows per statement where the application has them together ({many}){transaction}.",
                capitalize(verb)
            )
        }
    };
    let indexes = match lever.unused.as_slice() {
        [] => String::new(),
        [one] => format!(
            " {one} has had no reads {}, yet every write keeps it up to date: dropping it (see unused-index) saves that work.",
            since_phrase(snapshot)
        ),
        many => format!(
            " {} have had no reads {}, yet every write keeps them up to date: dropping them (see unused-index) saves that work.",
            format::list(many),
            since_phrase(snapshot)
        ),
    };
    format!("{opening} {how}{indexes}")
}

/// What to do about a read whose runs mostly find nothing. Caching its answer isn't one: "not there" stops being true
/// the moment the row is written. Where its plan seeks a unique index, that index is named: it is the one an insert
/// handling the duplicate-key error relies on, and it exists already.
fn finds_nothing(snapshot: &Snapshot, q: &QueryStat) -> String {
    let unique = q.plan.as_ref().and_then(|plan| {
        plan.nodes().into_iter().filter(|n| n.kind == NodeKind::Seek).find_map(|n| {
            let (schema, table) = crate::indexes::split_object(n.object.as_deref()?)?;
            let name = n.index.as_deref()?.trim_matches(['[', ']', '"', '`']);
            let index = snapshot
                .indexes_on(&schema, &table)
                .find(|i| i.name.eq_ignore_ascii_case(name) && (i.is_unique || i.is_primary_key) && !i.is_disabled)?;
            Some(match index.filter.as_deref() {
                Some(filter) => format!("{} on {schema}.{table} (unique for the rows where `{filter}`)", index.name),
                None => format!("{} on {schema}.{table}", index.name),
            })
        })
    });
    let instead = match unique {
        Some(index) => format!(
            "this plan seeks {index}, a unique index that exists already, so an insert that handles its duplicate-key error (or an upsert) needs no read first"
        ),
        None => "with a unique index on those columns, an insert that handles the duplicate-key error (or an upsert) needs no read first".to_string(),
    };
    format!(
        "Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: {instead}. If it polls for work, ask less often when it finds none, or have the writer notify the poller."
    )
}

/// Whether a read's runs mostly return no row.
fn finds_nothing_mostly(q: &QueryStat, kind: StatementKind) -> bool {
    kind == StatementKind::Read && q.rows_per_execution().is_some_and(|r| r < 0.5)
}

/// The fix for a read whose runs spend most of their time waiting for storage. Waiting alone cannot distinguish
/// cache pressure, cold pages or storage latency; preserve that uncertainty before suggesting more memory.
fn storage_fix(snapshot: &Snapshot, q: &QueryStat, cheap_plan: bool, finds_nothing_mostly: bool) -> String {
    let engine = snapshot.server.engine;
    let (verb, unit) = if engine == Engine::Mysql { ("examines", "rows") } else { ("reads", "pages") };
    let reads = q.reads_per_execution().map(|r| format::count(r.round().max(1.0)));
    let opening = match (cheap_plan, reads) {
        (true, Some(reads)) => {
            format!("Each run {verb} about {reads} {unit} in total, and most of its elapsed time is storage waiting.")
        }
        (_, Some(reads)) => format!(
            "Each run {verb} about {reads} {unit} in total, and most of its elapsed time is storage waiting. Check whether it reads unnecessary rows or columns."
        ),
        (_, None) => "Most of its elapsed time is storage waiting.".to_string(),
    };
    let mut fix = format!(
        "{opening} Compare physical reads and storage latency under representative load to distinguish slow I/O, a cold cache and repeated cache misses. The wait share alone does not establish eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change."
    );
    if finds_nothing_mostly {
        fix.push(' ');
        fix.push_str(&finds_nothing(snapshot, q));
    } else if cheap_plan {
        fix.push_str(" Fewer runs (a result cached where it's safe to reuse, calls batched) also save the reads.");
    }
    fix
}

/// A seek followed by a filter, beside a statement that reads many pages and returns little. The plan's estimates
/// and the statement's recorded reads are separate evidence: pages cannot establish how many rows a lookup handled.
pub(super) struct ResidualRead<'a> {
    /// The seek, and the step that checks each row it finds: the seek itself, or a lookup into the table.
    seek: &'a PlanNode,
    pub(super) check: &'a PlanNode,
    pub(super) schema: String,
    pub(super) table: String,
    /// The columns it seeks on, and those it checks afterwards.
    sought: Vec<String>,
    checked: Vec<String>,
    /// The engine of the plan, the pages (rows examined, MySQL) a run reads, and what a run returns.
    engine: Engine,
    reads: f64,
    returns: Returns,
}

pub(super) fn residual_read(plan: &Plan, reads: f64, returns: Returns) -> Option<ResidualRead<'_>> {
    let nodes = plan.nodes();
    let (seek, check) = nodes
        .iter()
        .copied()
        .filter(|n| n.kind == NodeKind::Lookup && n.op.contains("Lookup") && n.predicate.is_some())
        .find_map(|lookup| {
            let seek = nodes.iter().copied().find(|s| {
                s.kind == NodeKind::Seek && s.index.is_some() && s.object.is_some() && s.object == lookup.object
            })?;
            Some((seek, lookup))
        })
        .or_else(|| {
            let seek = nodes.iter().copied().find(|s| {
                s.kind == NodeKind::Seek && s.index.is_some() && s.object.is_some() && s.predicate.is_some()
            })?;
            Some((seek, seek))
        })?;
    let mut keys = predicate_columns(seek.seek_predicate.as_deref().unwrap_or_default());
    keys.equality.append(&mut keys.range);
    let checked_columns = predicate_columns(check.predicate.as_deref()?);
    let mut checked = checked_columns.equality.clone();
    checked.extend(checked_columns.range.into_iter().take(1));
    if keys.equality.is_empty()
        || checked.is_empty()
        || checked.iter().any(|c| keys.equality.iter().any(|k| k.eq_ignore_ascii_case(c)))
    {
        return None;
    }
    let (schema, table) = crate::indexes::split_object(seek.object.as_deref()?)?;
    Some(ResidualRead {
        seek,
        check,
        schema,
        table,
        sought: keys.equality,
        checked,
        engine: plan.engine,
        reads,
        returns,
    })
}

impl ResidualRead<'_> {
    /// What the plan estimates of the seek and its check, beside what the statement's runs read and return.
    pub(super) fn evidence(&self) -> String {
        let seek = self.seek;
        let index = seek.index.as_deref().unwrap_or_default().trim_matches(['[', ']', '"', '`']);
        let expected = seek.est_rows_total().round().max(1.0);
        let (rows, sought, checked) =
            (format::count(expected), format::list(&self.sought), format::list(&self.checked));
        let shape = if std::ptr::eq(seek, self.check) {
            t!(
                "check.heavy_query.lookups.checks",
                rows = rows,
                count = expected,
                index = index,
                sought = sought,
                checked = checked
            )
        } else {
            t!(
                "check.heavy_query.lookups.looks_up",
                rows = rows,
                count = expected,
                index = index,
                sought = sought,
                checked = checked
            )
        };
        // MySQL counts the rows a statement examined, the other engines the pages it read.
        let (reads, returns) = (format::count(self.reads), self.returns.select());
        let runs = if self.engine == Engine::Mysql {
            t!("check.heavy_query.lookups.runs_rows_examined", reads = reads, returns = returns)
        } else {
            t!("check.heavy_query.lookups.runs_pages", reads = reads, returns = returns)
        };
        [shape, runs].join(" ")
    }
}

/// What to measure before changing anything for lookups whose row counts only an actual plan can establish: those
/// counts, and the IO on `schema.table` in the same run.
fn measure_fix(snapshot: &Snapshot, schema: &str, table: &str) -> Fix {
    let capture = if snapshot.server.engine.spec().abilities().last_actual_plan {
        "Read a retained actual plan when the server has one; otherwise capture a representative actual plan in a test environment."
    } else {
        "Capture an actual execution plan for a representative run in a test environment."
    };
    advice(format!(
        "{capture} Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on {schema}.{table}. Use that evidence to decide whether a change to statistics or the access path is justified."
    ))
}

/// What to change for a statement the driver or the connection pool sends ([`culprit_core::sql::is_session_statement`]):
/// the pool's or the driver's settings, never the application's code, and each kind of statement has its own.
pub(super) fn session_fix(engine: Engine, text: &str) -> String {
    let pool = "The driver or the connection pool sends this around the application's statements, not the application's own code";
    let command = first_keyword(text);
    match command.as_str() {
        "begin" | "start" | "commit" | "rollback" | "end" | "savepoint" | "release" => format!(
            "{pool}: check whether it wraps every statement in a transaction of its own (each commit waits for the log to reach disk), and run the statements that belong together in one transaction."
        ),
        "select" => format!(
            "{pool}: it checks that a connection is still alive. Check how often the pool does that (on every checkout, or on a timer) and check less often."
        ),
        _ => engine.spec().session_advice(&command).map(str::to_string).unwrap_or_else(|| format!(
            "{pool}: look at what the pool runs each time it hands out a connection (session settings, a reset), and set what the application needs once instead: in the connection string, or as the role's or the server's defaults."
        )),
    }
}

/// A statement's first keyword, past leading comments, lowercased.
fn first_keyword(text: &str) -> String {
    let mut rest = text.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("").to_ascii_lowercase()
}
