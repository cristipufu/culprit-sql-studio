use super::*;
use culprit_core::t;

/// Where a statement's time goes when it isn't reading or computing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdleKind {
    /// It calls a sleep function: the pause is what it was asked to do.
    Sleep,
    /// It waits for an application lock (GET_LOCK, pg_advisory_lock, sp_getapplock).
    AppLock,
    /// Locks other transactions hold.
    Locks,
    /// The application reading its results.
    Client,
    /// A memory grant.
    Memory,
    /// Pages other sessions are using (latches).
    Latch,
    /// Something the engine doesn't name.
    Unknown,
}

pub(in crate::checks::queries) struct Idle {
    pub(super) kind: IdleKind,
    /// What shows it ([`Idle::evidence`] words it).
    shown: Shown,
    pub(super) confidence: Confidence,
    /// The engine named the cause (Query Store's waits, MySQL's lock time) rather than Culprit inferring it.
    pub(super) measured: bool,
    /// The wait's share of the statement's time, where that is what named it: "most of it" only from half on.
    pub(super) share: Option<f64>,
}

/// What shows where a statement's time goes: the facts its evidence states.
enum Shown {
    /// It calls this sleep function.
    Sleep(&'static str),
    /// It calls this application lock.
    AppLock(&'static str),
    /// MySQL's own lock time, this share of its time.
    LockTime(f64),
    /// Query Store's waits for it: this much of this wait, beside this share of its time on CPU.
    QueryStoreWait { cpu: f64, wait_ms: f64, wait: String },
    /// This share of its time on CPU, and reads that take far less than a run.
    LittleCpu(f64),
    /// Few reads, yet a long run, and MySQL counted its lock time without it being most of the run: waiting, for what
    /// MySQL doesn't say.
    Unnamed,
    /// Few reads and no CPU time, yet a long run: most likely waiting for this lock.
    RowLock(LockFor),
}

/// The lock a write, or a read that locks the rows it finds, most likely waited for.
enum LockFor {
    /// The one its locking clause takes on the rows it finds.
    Clause(&'static str),
    /// The one a locking read takes on the rows it finds, by a clause its text doesn't show.
    LockingRead,
    /// A row lock another transaction held.
    Row,
}

impl Idle {
    /// What the title says it does instead of working: "most of it waiting for locks".
    pub(super) fn title_tail(&self) -> String {
        // "Most of it" from half on; where the engine didn't name the cause, "most likely".
        let share = self.share.filter(|s| *s < 0.5).map(format::percent);
        let part = if share.is_some() { "share" } else { "most" };
        let share = share.unwrap_or_default();
        match self.kind {
            IdleKind::Sleep => t!("check.heavy_query.idle.title.sleep"),
            IdleKind::AppLock => t!("check.heavy_query.idle.title.app_lock"),
            IdleKind::Locks => {
                let part = if self.measured { part } else { "likely" };
                t!("check.heavy_query.idle.title.locks", part = part, share = share)
            }
            IdleKind::Client => t!("check.heavy_query.idle.title.client", part = part, share = share),
            IdleKind::Memory => t!("check.heavy_query.idle.title.memory", part = part, share = share),
            IdleKind::Latch => t!("check.heavy_query.idle.title.latch", part = part, share = share),
            IdleKind::Unknown => t!("check.heavy_query.idle.title.unknown"),
        }
    }

    /// One or two sentences: what shows it. Its evidence rests on figures the source keeps only in its totals (reads a
    /// run, its run time, its waits): where the summary leads with Culprit's scans, it says what they are over
    /// ([`crate::support::totals_basis`]).
    pub(super) fn evidence(&self, snapshot: &Snapshot, q: &QueryStat) -> String {
        let engine = snapshot.server.engine;
        let run = format::duration(q.mean_time_ms);
        // MySQL counts the rows a statement examined, the other engines the pages it read.
        let reads = q
            .reads_per_execution()
            .map(|r| {
                let reads = format::count(r);
                if engine == Engine::Mysql {
                    t!("check.heavy_query.idle.reads_per_run_rows_examined", reads = reads)
                } else {
                    t!("check.heavy_query.idle.reads_per_run_pages", reads = reads)
                }
            })
            .unwrap_or_default();
        let basis = crate::support::totals_basis(snapshot, q);
        match &self.shown {
            Shown::Sleep(name) => t!("check.heavy_query.idle.evidence.sleep", name = *name),
            Shown::AppLock(name) => t!("check.heavy_query.idle.evidence.app_lock", name = *name),
            Shown::LockTime(share) => {
                let (engine, share, time) =
                    (snapshot.server.name(), format::percent(*share), format::duration(q.total_time_ms));
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.lock_time_over",
                        basis = basis,
                        engine = engine,
                        share = share,
                        time = time
                    ),
                    None => {
                        t!("check.heavy_query.idle.evidence.lock_time", engine = engine, share = share, time = time)
                    }
                }
            }
            Shown::QueryStoreWait { cpu, wait_ms, wait } => {
                let (cpu, wait_time) = (format::percent(*cpu), format::duration(*wait_ms));
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.query_store_wait_over",
                        basis = basis,
                        cpu = cpu,
                        wait_time = wait_time,
                        wait = wait
                    ),
                    None => t!(
                        "check.heavy_query.idle.evidence.query_store_wait",
                        cpu = cpu,
                        wait_time = wait_time,
                        wait = wait
                    ),
                }
            }
            Shown::LittleCpu(cpu) => {
                let cpu = format::percent(*cpu);
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.little_cpu_over",
                        basis = basis,
                        cpu = cpu,
                        reads = reads,
                        run = run
                    ),
                    None => t!("check.heavy_query.idle.evidence.little_cpu", cpu = cpu, reads = reads, run = run),
                }
            }
            Shown::Unnamed => {
                let engine = snapshot.server.name();
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.unnamed_over",
                        basis = basis,
                        reads = reads,
                        run = run,
                        engine = engine
                    ),
                    None => t!("check.heavy_query.idle.evidence.unnamed", reads = reads, run = run, engine = engine),
                }
            }
            Shown::RowLock(lock) => {
                // What says a statement waited, and doesn't say for what.
                let statistics = match engine {
                    Engine::Postgres => "pg_stat_statements",
                    Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => "statistics",
                };
                let (lock, clause) = match lock {
                    LockFor::Clause(clause) => ("clause", *clause),
                    LockFor::LockingRead => ("locking_read", ""),
                    LockFor::Row => ("row", ""),
                };
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.row_lock_over",
                        basis = basis,
                        reads = reads,
                        run = run,
                        lock = lock,
                        clause = clause,
                        statistics = statistics
                    ),
                    None => t!(
                        "check.heavy_query.idle.evidence.row_lock",
                        reads = reads,
                        run = run,
                        lock = lock,
                        clause = clause,
                        statistics = statistics
                    ),
                }
            }
        }
    }

    pub(super) fn verify(&self) -> Expect {
        match self.kind {
            IdleKind::Sleep | IdleKind::AppLock => Expect::TimeOrStops,
            _ => Expect::Average,
        }
    }
}

impl IdleKind {
    /// What to do about where the time goes. Where it waits for a lock, how to find the holder is the one every check
    /// that names a lock wait gives ([`find_lock_holder`]): Live only for waits that come often enough to catch.
    pub(super) fn fix(self, snapshot: &Snapshot, q: &QueryStat) -> Fix {
        let holder = || find_lock_holder(snapshot, q, None);
        let (summary, sql) = match self {
            IdleKind::AppLock => {
                let LockHolder { lead, sql } = holder();
                (
                    format!(
                        "{lead} Shorten the work done while the lock is held, or take the lock without waiting (a try-lock, or a lock timeout) and retry later."
                    ),
                    sql,
                )
            }
            IdleKind::Locks => {
                let LockHolder { lead, sql } = holder();
                (
                    format!(
                        "{lead} Keep those transactions short: commit sooner, and don't wait for a user or another service inside a transaction. An index or a rewrite of this statement won't shorten the wait."
                    ),
                    sql,
                )
            }
            _ => (self.advice(), None),
        };
        Fix { summary, sql, cost: None }
    }

    fn advice(self) -> String {
        match self {
            IdleKind::Sleep => "Find who sends it (a health check, a test, a retry loop with a back-off) and whether the pause belongs in the database at all: while it sleeps it holds a connection, and any locks its transaction took.".into(),
            IdleKind::AppLock | IdleKind::Locks => String::new(),
            IdleKind::Client => "The application reads the results slowly, or row by row while it does other work. Read all rows first and process them afterwards, or fetch fewer.".into(),
            IdleKind::Memory => "It waits for a memory grant while other queries hold the memory: find the queries with the largest grants (Health) and reduce them, usually with better estimates or an index that avoids a big sort or hash.".into(),
            IdleKind::Latch => "Sessions queue for the same pages: typically inserts at the end of an index on an increasing key, or tempdb allocation. See the hot-rows and last-page findings, and Live while it happens.".into(),
            IdleKind::Unknown => "Find what it waits for while it runs: Live shows each session's wait and any blocking chain. Typical causes are locks other transactions hold, a slow client reading the results, or a call outside the database.".into(),
        }
    }
}

/// Sleep functions, by the name to show.
const SLEEPS: [(&str, &str); 6] = [
    ("pg_sleep(", "pg_sleep"),
    ("pg_sleep_for(", "pg_sleep_for"),
    ("pg_sleep_until(", "pg_sleep_until"),
    ("sleep(", "SLEEP"),
    ("waitfor delay", "WAITFOR"),
    ("waitfor time", "WAITFOR"),
];

/// Application locks a statement waits for until another session releases them.
const APP_LOCKS: [(&str, &str); 5] = [
    ("get_lock(", "GET_LOCK"),
    ("pg_advisory_lock(", "pg_advisory_lock"),
    ("pg_advisory_xact_lock(", "pg_advisory_xact_lock"),
    ("pg_advisory_lock_shared(", "pg_advisory_lock_shared"),
    ("sp_getapplock", "sp_getapplock"),
];

/// Runs this long or longer are worth explaining by what they wait for.
const IDLE_MIN_MS: f64 = 100.0;
/// Generous costs of reading: a page from a slow disk, a row examined from one (MySQL). Reads that can't take a
/// tenth of a run's time even at these rates don't explain it.
const PAGE_READ_MS: f64 = 10.0;
const ROW_READ_MS: f64 = 1.0;
/// CPU under this share of the time: the statement isn't computing for most of it.
const CPU_SMALL: f64 = 0.2;
/// Pages (rows examined, MySQL) a run of a read that returns nothing reads before its title says so: many times what a
/// lookup that finds nothing reads (an index's few levels).
pub(super) const NOTHING_READS: f64 = 100.0;
/// A wait that takes at least this share of a statement's time is what its runs spend most of their time on.
const WAIT_MOST: f64 = 0.5;

/// What a statement's dominant wait is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitOn {
    /// Reading pages from storage: they weren't in memory.
    Storage,
    Locks,
    Latches,
    /// Its changes reaching the transaction log.
    Log,
    /// The application reading the results.
    Client,
    Memory,
    /// Compiling its plan.
    Compilation,
}

/// The wait that takes most of a statement's time by the engine's own count of its waits ([`dominant_wait`]).
pub(super) struct DominantWait<'a> {
    pub(super) on: WaitOn,
    /// Its share of the statement's time. Query Store sums a parallel plan's waits over its threads, which can pass the
    /// statement's time: then it is "most of" it.
    pub(super) share: f64,
    /// The engine's name for it: Query Store's wait type, or its category.
    name: &'a str,
}

impl DominantWait<'_> {
    /// In a sentence: "85% of its time is spent waiting for data to come from storage because it wasn't in memory (Query
    /// Store: Buffer IO)."
    pub(super) fn sentence(&self, snapshot: &Snapshot, q: &QueryStat) -> String {
        // PostgreSQL's time on storage is its time reading and writing data files.
        let on = match self.on {
            WaitOn::Storage if snapshot.server.engine == Engine::Postgres => "storage_files",
            WaitOn::Storage => "storage",
            WaitOn::Locks => "locks",
            WaitOn::Latches => "latches",
            WaitOn::Log => "log",
            WaitOn::Client => "client",
            WaitOn::Memory => "memory",
            WaitOn::Compilation => "compilation",
        };
        let part = if self.share >= 1.0 { "most" } else { "share" };
        let share = format::percent(self.share);
        let source =
            snapshot.workload.as_ref().map_or_else(|| t!("check.heavy_query.the_statistics"), |w| w.source.clone());
        // A part of its time the source keeps only in its totals: said over them where the share is the scans'.
        match crate::support::totals_basis(snapshot, q) {
            Some(basis) => t!(
                "check.heavy_query.wait.sentence_over",
                basis = basis,
                part = part,
                share = share,
                on = on,
                source = source,
                name = self.name
            ),
            None => t!(
                "check.heavy_query.wait.sentence",
                part = part,
                share = share,
                on = on,
                source = source,
                name = self.name
            ),
        }
    }
}

/// The wait that takes most of a statement's time by the engine's own count of its waits (Query Store's per query,
/// PostgreSQL's time reading and writing data files, MySQL's lock time). Not the statement's own work (CPU, a parallel
/// plan's threads waiting for each other), nor a category with no plain meaning.
pub(super) fn dominant_wait(q: &QueryStat) -> Option<DominantWait<'_>> {
    if q.total_time_ms <= 0.0 {
        return None;
    }
    let w = q
        .waits
        .iter()
        .filter(|w| w.wait_ms >= q.total_time_ms * WAIT_MOST)
        .max_by(|a, b| a.wait_ms.total_cmp(&b.wait_ms))?;
    let on = match culprit_core::waits::category_key(&w.category) {
        "io" => WaitOn::Storage,
        "lock" => WaitOn::Locks,
        "latch" => WaitOn::Latches,
        "log" => WaitOn::Log,
        "network" => WaitOn::Client,
        "memory" => WaitOn::Memory,
        "compilation" => WaitOn::Compilation,
        _ => return None,
    };
    Some(DominantWait { on, share: w.wait_ms / q.total_time_ms, name: w.wait_type.as_deref().unwrap_or(&w.category) })
}

/// Where a statement's time goes when it isn't work (`idle_time`), for the views that word a finding by it (the brief).
pub(crate) fn idle_kind(snapshot: &Snapshot, q: &QueryStat) -> Option<IdleKind> {
    idle_time(snapshot, q).map(|i| i.kind)
}

/// When a statement's time isn't work: it sleeps, waits for a lock, or does too little reading and computing to fill
/// its run time. Uses the engine's own account (Query Store waits, MySQL lock time) where there is one.
pub(in crate::checks::queries) fn idle_time(snapshot: &Snapshot, q: &QueryStat) -> Option<Idle> {
    let text = squeeze(&q.text);
    let named = |list: &[(&str, &'static str)]| -> Option<&'static str> {
        list.iter()
            .find(|(pattern, _)| if pattern.ends_with('(') { calls(&text, pattern) } else { text.contains(pattern) })
            .map(|(_, name)| *name)
    };
    if let Some(name) = named(&SLEEPS) {
        return Some(Idle {
            kind: IdleKind::Sleep,
            shown: Shown::Sleep(name),
            confidence: Confidence::High,
            measured: true,
            share: None,
        });
    }
    if let Some(name) = named(&APP_LOCKS) {
        return Some(Idle {
            kind: IdleKind::AppLock,
            shown: Shown::AppLock(name),
            confidence: Confidence::High,
            measured: true,
            share: None,
        });
    }
    if q.mean_time_ms < IDLE_MIN_MS || q.total_time_ms <= 0.0 {
        return None;
    }
    let engine = snapshot.server.engine;
    let read_cost = if engine == Engine::Mysql { ROW_READ_MS } else { PAGE_READ_MS };
    let reads = q.reads_per_execution();
    let reads_small =
        reads.is_some_and(|r| r * read_cost < q.mean_time_ms * 0.1) && q.temp_writes.is_none_or(|t| t <= 0.0);
    let cpu_share = q.cpu_time_ms.map(|c| c / q.total_time_ms);
    let cpu_small = cpu_share.is_some_and(|s| s < CPU_SMALL);

    // MySQL's own lock time (row locks too from 8.0.28; the slow query log's lock time).
    let lock_ms = q.spread.as_ref().and_then(|s| s.lock_ms).or_else(|| {
        (engine == Engine::Mysql).then(|| q.waits.iter().filter(|w| w.category == "Lock").map(|w| w.wait_ms).sum())
    });
    if engine == Engine::Mysql
        && let Some(lock) = lock_ms.filter(|l| *l >= q.total_time_ms * 0.5)
    {
        return Some(Idle {
            kind: IdleKind::Locks,
            shown: Shown::LockTime((lock / q.total_time_ms).min(1.0)),
            confidence: Confidence::High,
            measured: true,
            share: None,
        });
    }

    // Query Store's waits per query (summed over parallel threads, so a category names the cause, not a share).
    if engine == Engine::Mssql && cpu_small {
        let top = q
            .waits
            .iter()
            .filter(|w| w.category != "CPU" && w.wait_ms >= q.total_time_ms * 0.25)
            .max_by(|a, b| a.wait_ms.total_cmp(&b.wait_ms));
        // Categories as the SQL Server provider folds Query Store's (`wait_category`).
        let kind = match top.map(|w| w.category.as_str()) {
            Some("Lock") => Some(IdleKind::Locks),
            Some("Network") => Some(IdleKind::Client),
            Some("Memory") => Some(IdleKind::Memory),
            Some("Latch") => Some(IdleKind::Latch),
            // Reading from disk, writing the log, compiling or a parallel plan's threads are the statement's own work.
            Some("IO" | "Log" | "Parallelism" | "Compilation") => return None,
            Some(_) | None => None,
        };
        let cpu = cpu_share.unwrap_or(0.0);
        if let (Some(kind), Some(w)) = (kind, top) {
            return Some(Idle {
                kind,
                shown: Shown::QueryStoreWait {
                    cpu,
                    wait_ms: w.wait_ms,
                    wait: w.wait_type.as_deref().unwrap_or(&w.category).to_string(),
                },
                confidence: Confidence::High,
                measured: true,
                // Summed over a parallel plan's threads it can pass the statement's time: then it is most of it.
                share: Some((w.wait_ms / q.total_time_ms).min(1.0)),
            });
        }
        if reads_small {
            return Some(Idle {
                kind: IdleKind::Unknown,
                shown: Shown::LittleCpu(cpu),
                confidence: Confidence::Medium,
                measured: false,
                share: None,
            });
        }
        return None;
    }
    if cpu_small && reads_small {
        return Some(Idle {
            kind: IdleKind::Unknown,
            shown: Shown::LittleCpu(cpu_share.unwrap_or(0.0)),
            confidence: Confidence::Medium,
            measured: false,
            share: None,
        });
    }
    // No CPU time (PostgreSQL, the slow query log): a write, or a read that locks the rows it finds, that reads a
    // handful of pages can't compute for seconds, so it waited, most likely for a row lock. A plain read could be
    // computing (functions), so it isn't called waiting.
    let kind = kind_of(q);
    if cpu_share.is_none() && reads_small && matches!(kind, StatementKind::Write | StatementKind::Locking) {
        // MySQL counted its lock time and it wasn't most of the run: waiting, but not for what MySQL calls locks.
        if engine == Engine::Mysql && lock_ms.is_some() {
            return Some(Idle {
                kind: IdleKind::Unknown,
                shown: Shown::Unnamed,
                confidence: Confidence::Medium,
                measured: false,
                share: None,
            });
        }
        // A locking read asks for the lock on every row it finds: that's the one it most likely waited for.
        let lock = match locking_clause(&text) {
            Some(clause) if kind == StatementKind::Locking => LockFor::Clause(clause),
            _ if kind == StatementKind::Locking => LockFor::LockingRead,
            _ => LockFor::Row,
        };
        return Some(Idle {
            kind: IdleKind::Locks,
            shown: Shown::RowLock(lock),
            confidence: Confidence::Medium,
            measured: false,
            share: None,
        });
    }
    None
}
