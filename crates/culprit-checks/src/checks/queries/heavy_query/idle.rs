use super::*;

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
    pub(super) fn title_tail(&self) -> String {
        let part = match self.share {
            Some(s) if s < 0.5 => format!("{} of it", format::percent(s)),
            _ => "most of it".to_string(),
        };
        let most = if self.measured { part.clone() } else { "most likely".to_string() };
        match self.kind {
            IdleKind::Sleep => "sleeping".into(),
            IdleKind::AppLock => "waiting for an application lock".into(),
            IdleKind::Locks => format!("{most} waiting for locks"),
            IdleKind::Client => format!("{part} waiting for the application to read the results"),
            IdleKind::Memory => format!("{part} waiting for memory"),
            IdleKind::Latch => format!("{part} waiting for pages other sessions are using"),
            IdleKind::Unknown => "most of it waiting rather than working".into(),
        }
    }

    /// One or two sentences: what shows it. Its evidence rests on figures the source keeps only in its totals (reads a
    /// run, its run time, its waits): where the summary leads with Culprit's scans, it says what they are over
    /// ([`over_totals`]).
    pub(super) fn evidence(&self, snapshot: &Snapshot, q: &QueryStat) -> String {
        let engine = snapshot.server.engine;
        let run = format::duration(q.mean_time_ms);
        let unit = if engine == Engine::Mysql { "rows examined" } else { "pages" };
        let reads_text =
            q.reads_per_execution().map(|r| format!("{} {unit} per run", format::count(r))).unwrap_or_default();
        let sentence = match &self.shown {
            Shown::Sleep(name) => {
                return format!(
                    "It calls {name}: the time is a deliberate pause, not work, and no index or rewrite makes it shorter."
                );
            }
            Shown::AppLock(name) => {
                return format!(
                    "It calls {name}, which waits until the session holding that lock releases it: its time is how long others hold the lock, not work it does."
                );
            }
            Shown::LockTime(share) => format!(
                "{} counts {} of its {} waiting for locks.",
                snapshot.server.name(),
                format::percent(*share),
                format::duration(q.total_time_ms)
            ),
            Shown::QueryStoreWait { cpu, wait_ms, wait } => format!(
                "Only {} of its time is CPU, and Query Store records {} of {wait} waits for it.",
                format::percent(*cpu),
                format::duration(*wait_ms)
            ),
            Shown::LittleCpu(cpu) => format!(
                "Only {} of its time is CPU, and {reads_text} take far less than {run}: most of each run is spent waiting.",
                format::percent(*cpu)
            ),
            Shown::Unnamed => format!(
                "It examined {reads_text}, which takes milliseconds, yet ran {run}: most of that time it was waiting, and {} doesn't say for what.",
                snapshot.server.name()
            ),
            Shown::RowLock(lock) => {
                let source = match engine {
                    Engine::Postgres => "pg_stat_statements doesn't record what a statement waited for",
                    Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
                        "the statistics don't record what it waited for"
                    }
                };
                let lock = match lock {
                    LockFor::Clause(clause) => {
                        format!("for the lock its {clause} takes on the rows it finds, which another transaction held")
                    }
                    LockFor::LockingRead => {
                        "for the lock it takes on the rows it finds, which another transaction held".to_string()
                    }
                    LockFor::Row => "for a row lock another transaction held".to_string(),
                };
                format!(
                    "It read {reads_text}, which takes milliseconds, yet ran {run}: almost all of that time it was waiting, most likely {lock} ({source})."
                )
            }
        };
        over_totals(snapshot, q, sentence)
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
        let doing = match self.on {
            WaitOn::Storage if snapshot.server.engine == Engine::Postgres => "reading and writing data files",
            WaitOn::Storage => "waiting for data to come from storage because it wasn't in memory",
            WaitOn::Locks => "waiting for locks other transactions hold",
            WaitOn::Latches => "waiting for pages other sessions are using",
            WaitOn::Log => "waiting for its changes to reach the transaction log",
            WaitOn::Client => "waiting for the application to read the results",
            WaitOn::Memory => "waiting for memory",
            WaitOn::Compilation => "compiling its plan",
        };
        let part = if self.share >= 1.0 {
            "Most of its time".to_string()
        } else {
            format!("{} of its time", format::percent(self.share))
        };
        let source = snapshot.workload.as_ref().map_or("the statistics", |w| w.source.as_str());
        // A part of its time the source keeps only in its totals: said over them where the share is the scans'.
        let sentence = format!("{part} is spent {doing} ({source}: {}).", self.name);
        over_totals(snapshot, q, sentence)
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

/// A sentence of figures from statement `q`'s totals, led by what they are over where its share and runs are said over
/// Culprit's scans ([`crate::support::totals_basis`]): "Over the 1277 days since statistics were reset, it read …".
fn over_totals(snapshot: &Snapshot, q: &QueryStat, sentence: String) -> String {
    let Some(basis) = crate::support::totals_basis(snapshot, q) else { return sentence };
    // A pronoun or an adverb leads in lower case after the basis; a name (MySQL) or a figure stays as it is.
    let lead = match sentence.split_whitespace().next() {
        Some("It" | "Only" | "Most") => {
            let mut chars = sentence.chars();
            chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
        }
        _ => sentence,
    };
    format!("Over {basis}, {lead}")
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
