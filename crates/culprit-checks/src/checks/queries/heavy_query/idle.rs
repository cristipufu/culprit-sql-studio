use super::super::recorded_waits::WaitSource;
use super::*;
use culprit_core::{WaitStat, engines::ReadUnit, t, telemetry::WaitClass};

/// A recorded wait class or direct evidence of a deliberate pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdleKind {
    /// It calls a sleep function: the pause is what it was asked to do.
    Sleep,
    /// It waits for an application lock (GET_LOCK, pg_advisory_lock, sp_getapplock).
    AppLock,
    /// Recorded lock waiting; its resource and holder still need identifying.
    Locks,
    /// Recorded network waiting; result consumption is one possible cause.
    Client,
    /// Recorded memory waiting; the native wait determines its cause.
    Memory,
    /// Recorded latch waiting; its resource still needs identifying.
    Latch,
    /// Something the engine doesn't name.
    Unknown,
}

pub(in crate::checks::queries) struct Idle {
    /// What shows it ([`Idle::evidence`] words it).
    shown: Shown,
    pub(super) confidence: Confidence,
}

/// What shows where a statement's time goes: the facts its evidence states.
enum Shown {
    /// It calls this sleep function.
    Sleep(&'static str),
    /// It calls this application lock.
    AppLock(&'static str),
    /// The source's explicit scalar lock total; its accounting is not an elapsed-time guarantee.
    LockTime(f64),
    /// A recorded native wait and its declared source, beside the measured CPU total.
    RecordedWait { source: WaitSource, cpu_ms: f64, wait_ms: f64, class: WaitClass, native_name: Option<String> },
    /// This share of its time on CPU, and reads that take far less than a run.
    LittleCpu(f64),
}

impl Idle {
    pub(super) fn kind(&self) -> IdleKind {
        match self.shown {
            Shown::Sleep(_) => IdleKind::Sleep,
            Shown::AppLock(_) => IdleKind::AppLock,
            Shown::LockTime(_) => IdleKind::Locks,
            Shown::RecordedWait { class: WaitClass::Lock, .. } => IdleKind::Locks,
            Shown::RecordedWait { class: WaitClass::Network, .. } => IdleKind::Client,
            Shown::RecordedWait { class: WaitClass::Memory, .. } => IdleKind::Memory,
            Shown::RecordedWait { class: WaitClass::Latch, .. } => IdleKind::Latch,
            Shown::RecordedWait { .. } | Shown::LittleCpu(_) => IdleKind::Unknown,
        }
    }

    /// Native wait totals are not exclusive portions of elapsed time, including in a parallel plan.
    pub(super) fn title_tail(&self, engine: Engine) -> String {
        match &self.shown {
            Shown::Sleep(_) => t!("check.heavy_query.idle.title.sleep"),
            Shown::AppLock(_) => t!("check.heavy_query.idle.title.app_lock"),
            Shown::LockTime(wait_ms) => t!(
                "check.heavy_query.idle.title.recorded_wait",
                wait_time = format::duration(*wait_ms),
                wait = wait_name(None, WaitClass::Lock, engine)
            ),
            Shown::RecordedWait { wait_ms, class, native_name, .. } => t!(
                "check.heavy_query.idle.title.recorded_wait",
                wait_time = format::duration(*wait_ms),
                wait = wait_name(native_name.as_deref(), *class, engine)
            ),
            Shown::LittleCpu(_) => t!("check.heavy_query.idle.title.unknown"),
        }
    }

    /// One or two sentences: what shows it. Its evidence rests on figures the source keeps only in its totals (reads a
    /// run, its run time, its waits): where the summary leads with Culprit's scans, it says what they are over
    /// ([`crate::support::totals_basis`]).
    pub(super) fn evidence(&self, snapshot: &Snapshot, q: &QueryStat) -> String {
        let engine = snapshot.server.engine;
        let run = format::duration(q.mean_time_ms);
        let read_unit = engine.spec().statement_metrics().reads;
        let reads = q
            .reads_per_execution()
            .map(|r| {
                let reads = format::count(r);
                match read_unit {
                    ReadUnit::RowsExamined => {
                        t!("check.heavy_query.idle.reads_per_run_rows_examined", n = r, reads = reads)
                    }
                    ReadUnit::Pages => t!("check.heavy_query.idle.reads_per_run_pages", n = r, reads = reads),
                }
            })
            .unwrap_or_default();
        let basis = crate::support::totals_basis(snapshot, q);
        match &self.shown {
            Shown::Sleep(name) => t!("check.heavy_query.idle.evidence.sleep", name = *name),
            Shown::AppLock(name) => t!("check.heavy_query.idle.evidence.app_lock", name = *name),
            Shown::LockTime(wait_ms) => {
                DominantWait { on: WaitOn::Locks, wait_ms: *wait_ms, native_name: None }.sentence(snapshot, q)
            }
            Shown::RecordedWait { source, cpu_ms, wait_ms, class, native_name } => {
                let (cpu_time, wait_time) = (format::duration(*cpu_ms), format::duration(*wait_ms));
                let wait = wait_name(native_name.as_deref(), *class, engine);
                let source = source.label(snapshot);
                match basis {
                    Some(basis) => t!(
                        "check.heavy_query.idle.evidence.recorded_wait_over",
                        source = source,
                        basis = basis,
                        cpu_time = cpu_time,
                        wait_time = wait_time,
                        wait = wait
                    ),
                    None => t!(
                        "check.heavy_query.idle.evidence.recorded_wait",
                        source = source,
                        cpu_time = cpu_time,
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
        }
    }

    pub(super) fn verify(&self) -> Expect {
        match self.shown {
            Shown::Sleep(_) | Shown::AppLock(_) => Expect::TimeOrStops,
            Shown::RecordedWait { .. } | Shown::LockTime(_) => Expect::RecordedWait,
            _ => Expect::Average,
        }
    }
}

impl IdleKind {
    /// What to do about where the time goes. Where it waits for a lock, how to find the holder is the one every check
    /// that names a lock wait gives ([`find_lock_holder`]): Live only for waits that come often enough to catch.
    pub(super) fn fix(self, snapshot: &Snapshot, q: &QueryStat) -> Fix {
        // A lock's holder is found first, then what to change.
        let after_holder = |then: String| {
            let LockHolder { lead, sql } = find_lock_holder(snapshot, q, None);
            ([lead, then].join(" "), sql)
        };
        let (summary, sql) = match self {
            IdleKind::AppLock => after_holder(t!("check.heavy_query.fix.idle.app_lock")),
            IdleKind::Locks => after_holder(t!("check.heavy_query.fix.idle.locks")),
            IdleKind::Sleep => (t!("check.heavy_query.fix.idle.sleep"), None),
            IdleKind::Client => (t!("check.heavy_query.fix.idle.client"), None),
            IdleKind::Memory => {
                (t!("check.heavy_query.fix.idle.memory", health = culprit_core::t!("core.dashboard.tile.health")), None)
            }
            IdleKind::Latch => (t!("check.heavy_query.fix.idle.latch"), None),
            IdleKind::Unknown => (t!("check.heavy_query.fix.idle.unknown"), None),
        };
        Fix { summary, sql, cost: None }
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
/// Materiality policy only: summed native waits need not be exclusive portions of elapsed time.
const MATERIAL_WAIT_RUNTIME_RATIO: f64 = 0.5;
const IDLE_WAIT_RUNTIME_RATIO: f64 = 0.25;

/// What a statement's dominant wait is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitOn {
    /// Recorded I/O; this class alone does not establish a cache miss or physical storage access.
    Storage,
    Locks,
    Latches,
    /// Its changes reaching the transaction log.
    Log,
    /// Recorded network waiting.
    Client,
    Memory,
    /// Compiling its plan.
    Compilation,
}

/// A material native wait total, without assuming exclusive elapsed-time accounting.
pub(super) struct DominantWait<'a> {
    pub(super) on: WaitOn,
    wait_ms: f64,
    /// An actual native name, when supplied. Portable class labels are localized when rendered.
    native_name: Option<&'a str>,
}

impl WaitOn {
    fn class(self) -> WaitClass {
        match self {
            Self::Storage => WaitClass::Io,
            Self::Locks => WaitClass::Lock,
            Self::Latches => WaitClass::Latch,
            Self::Log => WaitClass::Log,
            Self::Client => WaitClass::Network,
            Self::Memory => WaitClass::Memory,
            Self::Compilation => WaitClass::Compilation,
        }
    }
}

fn wait_name(native_name: Option<&str>, class: WaitClass, engine: Engine) -> String {
    native_name.map(str::to_owned).unwrap_or_else(|| culprit_core::waits::category_info(class.key(), engine).0)
}

fn wait_class(wait: &WaitStat) -> Option<WaitClass> {
    let key = culprit_core::waits::known_category_key(&wait.category)?;
    WaitClass::ALL.iter().copied().find(|class| class.key() == key)
}

impl DominantWait<'_> {
    /// Preserve the native total and name without deriving its cause or a portion of elapsed time.
    pub(super) fn sentence(&self, snapshot: &Snapshot, q: &QueryStat) -> String {
        let source =
            snapshot.workload.as_ref().map_or_else(|| t!("check.heavy_query.the_statistics"), |w| w.source_name());
        let name = wait_name(self.native_name, self.on.class(), snapshot.server.engine);
        let wait_time = format::duration(self.wait_ms);
        // The wait total remains over its native population even when the lead describes Culprit's own scans.
        match crate::support::totals_basis(snapshot, q) {
            Some(basis) => t!(
                "check.heavy_query.wait.sentence_over",
                basis = basis,
                wait_time = wait_time,
                source = source,
                name = name
            ),
            None => t!("check.heavy_query.wait.sentence", wait_time = wait_time, source = source, name = name),
        }
    }
}

/// The largest recognized material wait total. Comparing it with runtime chooses what to explain; it does not
/// establish a portion of elapsed time, a physical resource or a blocking owner.
pub(super) fn dominant_wait(q: &QueryStat) -> Option<DominantWait<'_>> {
    if !q.total_time_ms.is_finite() || q.total_time_ms <= 0.0 {
        return None;
    }
    q.waits
        .iter()
        .filter(|w| {
            w.wait_ms.is_finite() && w.wait_ms > 0.0 && w.wait_ms >= q.total_time_ms * MATERIAL_WAIT_RUNTIME_RATIO
        })
        .filter_map(|w| {
            let on = match wait_class(w)? {
                WaitClass::Io => WaitOn::Storage,
                WaitClass::Lock => WaitOn::Locks,
                WaitClass::Latch => WaitOn::Latches,
                WaitClass::Log => WaitOn::Log,
                WaitClass::Network => WaitOn::Client,
                WaitClass::Memory => WaitOn::Memory,
                WaitClass::Compilation => WaitOn::Compilation,
                _ => return None,
            };
            Some(DominantWait { on, wait_ms: w.wait_ms, native_name: w.wait_type.as_deref() })
        })
        .max_by(|a, b| a.wait_ms.total_cmp(&b.wait_ms))
}

/// Where a statement's time goes when it isn't work (`idle_time`), for the views that word a finding by it (the brief).
pub(crate) fn idle_kind(snapshot: &Snapshot, q: &QueryStat) -> Option<IdleKind> {
    idle_time(snapshot, q).map(|i| i.kind())
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
        return Some(Idle { shown: Shown::Sleep(name), confidence: Confidence::High });
    }
    if let Some(name) = named(&APP_LOCKS) {
        return Some(Idle { shown: Shown::AppLock(name), confidence: Confidence::High });
    }
    if !q.mean_time_ms.is_finite()
        || q.mean_time_ms < IDLE_MIN_MS
        || !q.total_time_ms.is_finite()
        || q.total_time_ms <= 0.0
    {
        return None;
    }
    let engine = snapshot.server.engine;
    let read_cost = match engine.spec().statement_metrics().reads {
        ReadUnit::RowsExamined => ROW_READ_MS,
        ReadUnit::Pages => PAGE_READ_MS,
    };
    let reads = q.reads_per_execution();
    let reads_small = reads.is_some_and(|r| r * read_cost < q.mean_time_ms * 0.1)
        && q.temporary_work.as_ref().is_none_or(|work| work.has_measurements() && !work.has_observed_work());
    let cpu_ms = q.cpu_time_ms.filter(|cpu| cpu.is_finite() && *cpu >= 0.0);
    let cpu_share = cpu_ms.map(|cpu| cpu / q.total_time_ms);

    // A scalar's storage shape does not establish exclusive elapsed-time accounting either. The comparison is a
    // materiality threshold only; retain its exact native total without clamping or assigning it to individual runs.
    if let Some(lock) = q
        .spread
        .as_ref()
        .and_then(|spread| spread.lock_ms)
        .filter(|lock| lock.is_finite() && *lock > 0.0 && *lock >= q.total_time_ms * 0.5)
    {
        return Some(Idle { shown: Shown::LockTime(lock), confidence: Confidence::High });
    }

    // Declared canonical wait classes carry meaning across sources. Current configuration and engine identity do
    // not assign the source's vocabulary, and absent CPU still cannot establish an idle cause.
    if let (Some(cpu), Some(cpu_ms)) = (cpu_share.filter(|share| share.is_finite() && *share < CPU_SMALL), cpu_ms) {
        let top = q
            .waits
            .iter()
            .filter(|w| {
                w.wait_ms.is_finite()
                    && w.wait_ms > 0.0
                    && wait_class(w) != Some(WaitClass::Cpu)
                    && w.wait_ms >= q.total_time_ms * IDLE_WAIT_RUNTIME_RATIO
            })
            .max_by(|a, b| a.wait_ms.total_cmp(&b.wait_ms));
        let class = top.and_then(wait_class);
        let explains_idle = match class {
            Some(WaitClass::Lock | WaitClass::Network | WaitClass::Memory | WaitClass::Latch) => true,
            // These classes need their own diagnosis, not a guessed idle cause.
            Some(WaitClass::Io | WaitClass::Log | WaitClass::Parallelism | WaitClass::Compilation) => return None,
            Some(_) | None => false,
        };
        if let (true, Some(w), Some(class)) = (explains_idle, top, class) {
            return Some(Idle {
                shown: Shown::RecordedWait {
                    source: WaitSource::of(snapshot),
                    cpu_ms,
                    wait_ms: w.wait_ms,
                    class,
                    native_name: w.wait_type.clone(),
                },
                confidence: Confidence::High,
            });
        }
        if reads_small {
            return Some(Idle { shown: Shown::LittleCpu(cpu), confidence: Confidence::Medium });
        }
        return None;
    }
    // Without a native CPU measurement, read counts and SQL shape cannot establish waiting or its cause.

    None
}
