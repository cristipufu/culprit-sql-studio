//! SQL Server configuration and Query Store health checks.

use super::*;

/// What SQL Server's settings establish, one finding each.
pub enum Case<'s> {
    QueryStoreOff,
    /// Query Store turned read-only for `reason` (its codes, added up), which decides the fix.
    QueryStoreReadOnly {
        reason: u64,
        fix: ReadOnlyFix,
        max: Option<f64>,
    },
    ReadCommittedSnapshot(Rcsi<'s>),
    /// AUTO_CREATE_STATISTICS (`create`) or AUTO_UPDATE_STATISTICS off.
    AutoStatistics {
        create: bool,
    },
    /// The database's compatibility level, two or more behind the newest the server (or the service: `cloud`) offers.
    CompatibilityLevel {
        level: u32,
        latest: u32,
        cloud: Option<&'s str>,
    },
    CostThreshold,
    /// Query Store 90% full or more, without size-based cleanup.
    QueryStoreSize {
        used: f64,
        max: f64,
    },
    /// Query Store capturing every statement: `busy` with 5,000 statements or more in the window.
    QueryStoreCapture {
        busy: bool,
        statements: Option<f64>,
    },
    QueryStoreWaits,
    LegacyCardinalityEstimation,
    /// MAXDOP 0 on more than 8 processors: the cap to set, and whether it can be set for this database alone.
    Maxdop {
        cpus: f64,
        nodes: f64,
        per_node: Option<f64>,
        cap: f64,
        scoped: bool,
    },
}

/// What brings a read-only Query Store back to recording, by why it stopped.
#[derive(Clone, Copy)]
pub enum ReadOnlyFix {
    /// It reached its size limit: more room, and cleanup.
    Room,
    /// It holds as many statements as its memory allows: automatic capture.
    Capture,
    /// The database is out of disk space.
    Disk,
    /// It usually returns to recording by itself.
    Wait,
}

pub(super) fn decide(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = settings(snapshot);
    out.extend(health(snapshot));
    out
}

fn settings(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    let qs_actual = value(snapshot, &["query_store_options.actual_state_desc"]);
    let qs_reason =
        value(snapshot, &["query_store_options.readonly_reason"]).and_then(|r| r.trim().parse::<u64>().ok());
    match qs_actual.map(|v| v.to_ascii_uppercase()) {
        Some(state) if state.contains("OFF") => out.push(Case::QueryStoreOff),
        Some(state) if state.contains("READ_ONLY") => {
            out.extend(query_store_read_only(snapshot, qs_reason.unwrap_or(0)));
        }
        _ => {}
    }

    if read_committed_snapshot_off(snapshot) {
        out.push(Case::ReadCommittedSnapshot(read_committed_snapshot(snapshot)));
    }

    for (name, create) in [("is_auto_create_stats_on", true), ("is_auto_update_stats_on", false)] {
        if value(snapshot, &[name]).is_some_and(|v| !is_on(v)) {
            out.push(Case::AutoStatistics { create });
        }
    }

    if let Some(level) = value(snapshot, &["compatibility_level"]).and_then(|v| v.parse::<u32>().ok()) {
        let latest = latest_compat(&snapshot.server);
        if level + 20 <= latest {
            let cloud =
                snapshot.server.cloud.as_deref().filter(|_| snapshot.server.platform().is_some_and(|p| p.is_azure()));
            out.push(Case::CompatibilityLevel { level, latest, cloud });
        }
    }

    // Read where the server's options can be read (not Azure SQL Database). The threshold decides which plans go
    // parallel, so where nothing can (MAXDOP 1, one processor) it changes nothing.
    let ctfp = value(snapshot, &["cost threshold for parallelism"]).and_then(|v| v.parse::<f64>().ok());
    if ctfp.is_some_and(|v| v <= 5.0) && !serial_only(snapshot) {
        out.push(Case::CostThreshold);
    }
    out
}

fn health(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    let qs_on = value(snapshot, &["query_store_options.actual_state_desc"])
        .is_some_and(|v| matches!(v.to_ascii_uppercase().as_str(), "READ_WRITE" | "READ_ONLY"));

    // With size-based cleanup on AUTO, Query Store trims its oldest data at 90% full (down to about 80%) and never
    // fills: nearly full is how it runs, not a warning.
    let cleans_itself = value(snapshot, &["query_store_options.size_based_cleanup_mode_desc"])
        .is_some_and(|v| v.eq_ignore_ascii_case("AUTO"));
    if qs_on
        && !cleans_itself
        && let (Some(used), Some(max)) = (
            number(snapshot, "query_store_options.current_storage_size_mb"),
            number(snapshot, "query_store_options.max_storage_size_mb"),
        )
        && max > 0.0
        && used / max >= 0.9
        && value(snapshot, &["query_store_options.actual_state_desc"])
            .is_some_and(|v| v.eq_ignore_ascii_case("READ_WRITE"))
    {
        out.push(Case::QueryStoreSize { used, max });
    }

    if qs_on
        && value(snapshot, &["query_store_options.query_capture_mode_desc"])
            .is_some_and(|v| v.eq_ignore_ascii_case("ALL"))
    {
        // The statements the scan read are those of its window, not everything the store holds: said as what ran in
        // it. A handful crowds nothing, so the cost is said as what ALL would do once one-off statements arrive.
        out.push(Case::QueryStoreCapture {
            busy: snapshot.workload.as_ref().is_some_and(|w| w.query_count >= 5000.0),
            statements: snapshot.workload.as_ref().map(|w| w.query_count).filter(|n| *n > 0.0),
        });
    }

    if qs_on
        && value(snapshot, &["query_store_options.wait_stats_capture_mode_desc"])
            .is_some_and(|v| v.eq_ignore_ascii_case("OFF"))
    {
        out.push(Case::QueryStoreWaits);
    }

    if value(snapshot, &["LEGACY_CARDINALITY_ESTIMATION"]).is_some_and(is_on) {
        out.push(Case::LegacyCardinalityEstimation);
    }

    // MAXDOP 0 lets a query use every processor. Up to 8 per NUMA node that is what Microsoft recommends anyway, so it
    // matters only on a bigger server, which the scan has to know.
    let server_maxdop = number(snapshot, "max degree of parallelism");
    let db_maxdop = number(snapshot, "MAXDOP");
    let cpus = snapshot.pressure.as_ref().and_then(|p| p.cpus);
    let nodes = snapshot.pressure.as_ref().and_then(|p| p.numa_nodes).unwrap_or(1.0).max(1.0);
    let per_node = cpus.map(|c| (c / nodes).floor());
    if server_maxdop == Some(0.0)
        && db_maxdop.is_none_or(|v| v == 0.0)
        && let Some(cpus) = cpus.filter(|c| *c > 8.0)
    {
        out.push(Case::Maxdop {
            cpus,
            nodes,
            per_node,
            cap: per_node.unwrap_or(8.0).clamp(1.0, 8.0),
            // Database-scoped configuration came with SQL Server 2016 (Azure SQL has it whatever version it reports);
            // before that the cap is the server's.
            scoped: snapshot.server.platform().is_some_and(|p| p.is_azure_sql())
                || culprit_core::version_at_least(&snapshot.server.version, [13, 0, 0]),
        });
    }
    out
}

pub(super) fn render(snapshot: &Snapshot, case: Case<'_>) -> Finding {
    let db = quote(Engine::Mssql, &snapshot.server.database);
    match case {
        Case::QueryStoreOff => setting_finding(
            "query-store",
            Severity::Medium,
            t!("check.config.query_store.title"),
            t!("check.config.query_store.summary"),
            Fix {
                summary: t!("check.config.query_store.fix"),
                sql: Some(format!(
                    "ALTER DATABASE {db} SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE, QUERY_CAPTURE_MODE = AUTO);"
                )),
                cost: Some(t!("check.config.query_store.fix_cost")),
            },
        ),
        Case::QueryStoreReadOnly { reason, fix, max } => query_store_read_only_finding(&db, reason, fix, max),
        Case::ReadCommittedSnapshot(rcsi) => read_committed_snapshot_finding(snapshot, &db, rcsi),
        Case::AutoStatistics { create } => {
            // Some products require statistics they create themselves: SharePoint's content databases run with
            // AUTO_CREATE_STATISTICS off by design.
            let label = if create { "AUTO_CREATE_STATISTICS" } else { "AUTO_UPDATE_STATISTICS" };
            let mut f = setting_finding(
                &label.to_ascii_lowercase(),
                if create { Severity::Medium } else { Severity::High },
                t!("check.config.auto_statistics.title", setting = label),
                t!("check.config.auto_statistics.summary"),
                Fix {
                    summary: t!("check.config.auto_statistics.fix"),
                    sql: Some(format!("ALTER DATABASE {db} SET {label} ON;")),
                    cost: None,
                },
            );
            if create {
                f.caveats.push(t!("check.config.auto_statistics.caveat.vendor"));
            }
            f
        }
        Case::CompatibilityLevel { level, latest, cloud } => {
            let (level, latest) = (level.to_string(), latest.to_string());
            let mut f = setting_finding(
                "compatibility-level",
                Severity::Low,
                t!("check.config.compatibility_level.title", level = &level),
                match cloud {
                    Some(cloud) => t!(
                        "check.config.compatibility_level.summary_service",
                        service = cloud,
                        latest = &latest,
                        level = &level
                    ),
                    None => t!("check.config.compatibility_level.summary", latest = &latest, level = &level),
                },
                Fix {
                    summary: t!("check.config.compatibility_level.fix"),
                    sql: Some(format!("ALTER DATABASE {db} SET COMPATIBILITY_LEVEL = {latest};")),
                    cost: None,
                },
            );
            f.confidence = Confidence::Low;
            f.metrics
                .push(Metric::said_text(culprit_core::key!("check.config.compatibility_level.metric.current"), level));
            f
        }
        Case::CostThreshold => {
            let change = server_option(&snapshot.server, "cost threshold for parallelism", "50");
            setting_finding(
                "cost-threshold-for-parallelism",
                Severity::Low,
                t!("check.config.cost_threshold.title"),
                t!("check.config.cost_threshold.summary"),
                Fix {
                    summary: [t!("check.config.cost_threshold.fix"), change.how].join(" "),
                    sql: change.sql,
                    cost: None,
                },
            )
        }
        Case::QueryStoreSize { used, max } => {
            let mut f = setting_finding(
                "query-store-size",
                Severity::Medium,
                t!("check.config.query_store_size.title", share = format::percent(used / max)),
                t!(
                    "check.config.query_store_size.summary",
                    used = used.round().to_string(),
                    max = max.round().to_string()
                ),
                Fix {
                    summary: t!("check.config.query_store_size.fix"),
                    sql: Some(format!(
                        "ALTER DATABASE {db} SET QUERY_STORE (MAX_STORAGE_SIZE_MB = {}, SIZE_BASED_CLEANUP_MODE = AUTO);",
                        ((max * 2.0) / 1024.0).ceil() * 1024.0
                    )),
                    cost: Some(t!("check.config.query_store_size.fix_cost")),
                },
            );
            f.confidence = Confidence::High;
            f.metrics.push(Metric::said(
                culprit_core::key!("check.config.query_store_size.metric.used"),
                used / max,
                Unit::Ratio,
            ));
            f
        }
        Case::QueryStoreCapture { busy, statements } => {
            let window = snapshot.workload.as_ref().map(|w| w.window_phrase()).unwrap_or_default();
            let summary = match statements {
                Some(n) if busy => {
                    t!("check.config.query_store_capture.summary.busy", count = format::count(n), window = &window)
                }
                Some(n) => t!(
                    "check.config.query_store_capture.summary.quiet",
                    n = n,
                    count = format::count(n),
                    window = &window
                ),
                None => t!("check.config.query_store_capture.summary.unknown"),
            };
            let mut f = setting_finding(
                "query-store-capture",
                if busy { Severity::Medium } else { Severity::Low },
                t!("check.config.query_store_capture.title"),
                summary,
                Fix {
                    summary: t!("check.config.query_store_capture.fix"),
                    sql: Some(format!("ALTER DATABASE {db} SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO);")),
                    cost: None,
                },
            );
            f.confidence = Confidence::Medium;
            f
        }
        Case::QueryStoreWaits => setting_finding(
            "query-store-waits",
            Severity::Low,
            t!("check.config.query_store_waits.title"),
            t!("check.config.query_store_waits.summary"),
            Fix {
                summary: t!("check.config.query_store_waits.fix"),
                sql: Some(format!("ALTER DATABASE {db} SET QUERY_STORE (WAIT_STATS_CAPTURE_MODE = ON);")),
                cost: None,
            },
        ),
        Case::LegacyCardinalityEstimation => {
            let mut f = setting_finding(
                "legacy-cardinality-estimation",
                Severity::Low,
                t!("check.config.legacy_cardinality_estimation.title"),
                t!("check.config.legacy_cardinality_estimation.summary"),
                Fix {
                    summary: t!("check.config.legacy_cardinality_estimation.fix"),
                    sql: Some("ALTER DATABASE SCOPED CONFIGURATION SET LEGACY_CARDINALITY_ESTIMATION = OFF;".into()),
                    cost: None,
                },
            );
            f.confidence = Confidence::Low;
            f
        }
        Case::Maxdop { cpus, nodes, per_node, cap, scoped } => {
            let processors = format::count(cpus);
            let fix = if scoped {
                Fix {
                    summary: t!("check.config.maxdop.fix_database"),
                    sql: Some(format!("ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP = {cap:.0};")),
                    cost: None,
                }
            } else {
                let change = setting_change(&snapshot.server, "max degree of parallelism", &format!("{cap:.0}"));
                Fix {
                    summary: [t!("check.config.maxdop.fix_server"), change.how].join(" "),
                    sql: change.sql,
                    cost: None,
                }
            };
            let mut f = setting_finding(
                "maxdop",
                Severity::Low,
                t!("check.config.maxdop.title", count = &processors),
                t!(
                    "check.config.maxdop.summary",
                    count = &processors,
                    nodes = if nodes > 1.0 { "many" } else { "one" },
                    per_node = format::count(per_node.unwrap_or(cpus))
                ),
                fix,
            );
            f.confidence = Confidence::Low;
            f.metrics.push(Metric::said(
                culprit_core::key!("check.config.maxdop.metric.processors"),
                cpus,
                Unit::Count,
            ));
            f
        }
    }
}

/// Whether nothing in this database can run in parallel: its own MAXDOP is 1, or it defers (0) to a server MAXDOP of
/// 1, or the server has a single processor.
fn serial_only(snapshot: &Snapshot) -> bool {
    let db_maxdop = number(snapshot, "MAXDOP");
    let server_maxdop = number(snapshot, "max degree of parallelism");
    let cpus = snapshot.pressure.as_ref().and_then(|p| p.cpus);
    db_maxdop == Some(1.0) || (db_maxdop.is_none_or(|d| d == 0.0) && server_maxdop == Some(1.0)) || cpus == Some(1.0)
}

/// Query Store's `readonly_reason` codes (they add up when several apply), and the message that says each.
const READ_ONLY_REASONS: [(u64, &str); 8] = [
    (1, culprit_core::key!("check.config.query_store_read_only.reason.database_read_only")),
    (2, culprit_core::key!("check.config.query_store_read_only.reason.single_user")),
    (4, culprit_core::key!("check.config.query_store_read_only.reason.emergency")),
    (8, culprit_core::key!("check.config.query_store_read_only.reason.secondary")),
    (65_536, culprit_core::key!("check.config.query_store_read_only.reason.size_limit")),
    (131_072, culprit_core::key!("check.config.query_store_read_only.reason.statements")),
    (262_144, culprit_core::key!("check.config.query_store_read_only.reason.flush_memory")),
    (524_288, culprit_core::key!("check.config.query_store_read_only.reason.disk_full")),
];

/// A read-only Query Store, by why. A read-only database, single-user or emergency mode, or a secondary replica keep
/// it read-only by design: nothing to fix in Query Store. The rest each have their own fix.
fn query_store_read_only(snapshot: &Snapshot, reason: u64) -> Option<Case<'_>> {
    const BY_DESIGN: u64 = 1 | 2 | 4 | 8;
    if reason == 0 || reason & BY_DESIGN != 0 {
        return None;
    }
    let max = value(snapshot, &["query_store_options.max_storage_size_mb"]).and_then(|v| v.trim().parse::<f64>().ok());
    let fix = if reason & 65_536 != 0 {
        ReadOnlyFix::Room
    } else if reason & 131_072 != 0 {
        ReadOnlyFix::Capture
    } else if reason & 524_288 != 0 {
        ReadOnlyFix::Disk
    } else {
        ReadOnlyFix::Wait
    };
    Some(Case::QueryStoreReadOnly { reason, fix, max })
}

fn query_store_read_only_finding(db: &str, reason: u64, fix: ReadOnlyFix, max: Option<f64>) -> Finding {
    let reasons: Vec<String> = READ_ONLY_REASONS
        .iter()
        .filter(|(code, _)| reason & code != 0)
        .map(|(_, why)| culprit_core::i18n::t(why, &[]))
        .collect();
    let why = if reasons.is_empty() {
        t!("check.config.query_store_read_only.reason.code", code = reason.to_string())
    } else {
        format::list(&reasons)
    };
    let (summary, sql, severity) = match fix {
        ReadOnlyFix::Room => {
            // Never less room than it has now.
            let room = max.map_or(2048.0, |m| (m * 2.0).max(2048.0));
            (
                t!("check.config.query_store_read_only.fix.room"),
                format!(
                    "ALTER DATABASE {db} SET QUERY_STORE (MAX_STORAGE_SIZE_MB = {room:.0}, SIZE_BASED_CLEANUP_MODE = AUTO, OPERATION_MODE = READ_WRITE);"
                ),
                Severity::Medium,
            )
        }
        ReadOnlyFix::Capture => (
            t!("check.config.query_store_read_only.fix.capture"),
            format!("ALTER DATABASE {db} SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO, OPERATION_MODE = READ_WRITE);"),
            Severity::Medium,
        ),
        ReadOnlyFix::Disk => (
            t!("check.config.query_store_read_only.fix.disk"),
            format!("ALTER DATABASE {db} SET QUERY_STORE (OPERATION_MODE = READ_WRITE);"),
            Severity::Medium,
        ),
        ReadOnlyFix::Wait => (
            t!("check.config.query_store_read_only.fix.wait"),
            format!("ALTER DATABASE {db} SET QUERY_STORE (OPERATION_MODE = READ_WRITE);"),
            Severity::Low,
        ),
    };
    setting_finding(
        "query-store",
        severity,
        t!("check.config.query_store_read_only.title"),
        t!("check.config.query_store_read_only.summary", why = why),
        Fix { summary, sql: Some(sql), cost: None },
    )
}

/// Whether READ_COMMITTED_SNAPSHOT is known to be off for this database (SQL Server): reads under read committed then
/// wait for the locks writes hold on their rows.
pub(crate) fn read_committed_snapshot_off(snapshot: &Snapshot) -> bool {
    snapshot.server.engine == Engine::Mssql
        && value(snapshot, &["is_read_committed_snapshot_on"]).is_some_and(|v| !is_on(v))
}

/// SQL shape identifies a candidate only. Aggregate lock waits establish neither the lock resource/mode nor the
/// session isolation, so they cannot establish that row versioning would remove the wait.
pub(crate) fn row_versioning_candidate(q: &culprit_core::QueryStat) -> bool {
    matches!(statement_handle_of(&q.text), Handle::Read(_))
        && crate::queries::kind_of(q) != crate::queries::StatementKind::Locking
}

/// Observed lock waiting on candidate read statements; its cause and the effect of changing isolation remain unknown.
fn observed_read_locks(snapshot: &Snapshot) -> (f64, Option<(&culprit_core::QueryStat, f64)>) {
    let waited: Vec<(&culprit_core::QueryStat, f64)> = app_queries(snapshot)
        .filter(|q| row_versioning_candidate(q))
        .filter_map(|q| {
            let ms = q.observed_wait_ms(culprit_core::telemetry::WaitClass::Lock).filter(|ms| *ms > 0.0)?;
            Some((q, ms))
        })
        .collect();
    let lock_ms: f64 = waited.iter().map(|(_, ms)| ms).sum();
    let longest = waited.into_iter().max_by(|a, b| a.1.total_cmp(&b.1));
    (lock_ms, longest)
}

/// The reads that waited on locks (see [`observed_read_locks`]), as their runs came: whether any ran steadily enough for a
/// rate a day, when they ran where they came in bursts ("on Oct 11, 13:56–14:19 UTC", for one read or several in one
/// stretch, as the workload words it), and whether all of them stopped running (history: their waits are behind them).
struct WaitingReads<'a> {
    reads: Vec<&'a culprit_core::QueryStat>,
    steady: bool,
    when: Option<String>,
    stopped: bool,
}

fn waiting_reads(snapshot: &Snapshot) -> WaitingReads<'_> {
    let reads: Vec<&culprit_core::QueryStat> = app_queries(snapshot)
        .filter(|q| row_versioning_candidate(q))
        .filter(|q| q.observed_wait_ms(culprit_core::telemetry::WaitClass::Lock).is_some_and(|ms| ms > 0.0))
        .collect();
    let w = snapshot.workload.as_ref();
    let steady = reads.iter().any(|q| w.is_some_and(|w| w.runs_per_day_of(q).is_some()));
    // One stretch for all of them: the earliest first run to the latest last, where every read came in one burst.
    let bursts: Vec<(String, String)> = reads
        .iter()
        .filter_map(|q| match w?.cadence(q) {
            culprit_core::Cadence::Burst { from, to, days, .. } if days.is_empty() => Some((from, to)),
            _ => None,
        })
        .collect();
    let when = (!reads.is_empty() && !steady && bursts.len() == reads.len())
        .then(|| {
            let at = |s: &String| crate::support::parse_epoch_seconds(s);
            let from =
                bursts.iter().min_by(|a, b| at(&a.0).partial_cmp(&at(&b.0)).unwrap_or(std::cmp::Ordering::Equal))?;
            let to =
                bursts.iter().max_by(|a, b| at(&a.1).partial_cmp(&at(&b.1)).unwrap_or(std::cmp::Ordering::Equal))?;
            let span = at(&to.1)? - at(&from.0)?;
            (span < 86_400.0).then(|| culprit_core::span_when(&from.0, &to.1))
        })
        .flatten()
        .or_else(|| match reads.as_slice() {
            [q] if !steady => w.and_then(|w| w.cadence(q).when()),
            _ => None,
        });
    let stopped = !reads.is_empty() && reads.iter().all(|q| crate::query_is_history(snapshot, q));
    WaitingReads { reads, steady, when, stopped }
}

const MATERIAL_READ_LOCK_MS: f64 = 60_000.0;
const MATERIAL_READ_LOCK_SHARE: f64 = 0.03;
const NEARLY_ALL_READ_LOCK_SHARE: f64 = 0.9;
const MOST_READ_LOCK_SHARE: f64 = 0.5;

#[derive(Clone, Copy)]
enum RcsiEvidence {
    ConfigurationOnly,
    AggregateReadLocks { severity: Severity },
}

#[derive(Clone, Copy)]
enum ReadLockConcentration<'s> {
    NearlyAll(&'s culprit_core::QueryStat),
    Most { query: &'s culprit_core::QueryStat, milliseconds: f64 },
    Distributed,
}

/// READ_COMMITTED_SNAPSHOT off, with separate configuration advice and observed lock symptoms.
pub struct Rcsi<'s> {
    evidence: RcsiEvidence,
    lock_ms: f64,
    share: f64,
    concentration: ReadLockConcentration<'s>,
    waiting: WaitingReads<'s>,
    /// Every captured candidate read has qualified lock measurements; no unrelated query can establish absence.
    read_waits_qualified: bool,
    per_day: Option<f64>,
    snapshot_isolation: bool,
}

fn read_committed_snapshot(snapshot: &Snapshot) -> Rcsi<'_> {
    let (lock_ms, longest) = observed_read_locks(snapshot);
    let share = snapshot.workload.as_ref().map(|w| w.share(lock_ms)).unwrap_or(0.0);
    let waiting = waiting_reads(snapshot);
    let real_amount = severity_for_time_of(snapshot, &waiting.reads, lock_ms, share) >= Severity::Medium;
    // Severity describes the observed symptom; it never establishes the blocking resource or a setting's cure.
    let material =
        lock_ms >= MATERIAL_READ_LOCK_MS && share >= MATERIAL_READ_LOCK_SHARE && real_amount && !waiting.stopped;
    let evidence = if lock_ms > 0.0 {
        RcsiEvidence::AggregateReadLocks { severity: if material { Severity::Medium } else { Severity::Low } }
    } else {
        RcsiEvidence::ConfigurationOnly
    };
    let concentration = match longest {
        Some((query, ms)) if ms >= lock_ms * NEARLY_ALL_READ_LOCK_SHARE => ReadLockConcentration::NearlyAll(query),
        Some((query, ms)) if ms >= lock_ms * MOST_READ_LOCK_SHARE => {
            ReadLockConcentration::Most { query, milliseconds: ms }
        }
        _ => ReadLockConcentration::Distributed,
    };
    let per_day = snapshot.workload.as_ref().filter(|_| waiting.steady).and_then(|w| w.per_day_measured(lock_ms));
    let mut reads = app_queries(snapshot).filter(|q| row_versioning_candidate(q));
    let lock_measured =
        |q: &culprit_core::QueryStat| q.measured_wait_ms(culprit_core::telemetry::WaitClass::Lock).is_some();
    let read_waits_qualified = reads.next().is_some_and(|first| lock_measured(first) && reads.all(lock_measured));
    Rcsi {
        evidence,
        lock_ms,
        share,
        concentration,
        waiting,
        read_waits_qualified,
        per_day,
        snapshot_isolation: value(snapshot, &["snapshot_isolation_state_desc"])
            .is_some_and(|v| v.eq_ignore_ascii_case("ON")),
    }
}

fn read_committed_snapshot_finding(snapshot: &Snapshot, db: &str, r: Rcsi<'_>) -> Finding {
    let window = snapshot
        .workload
        .as_ref()
        .map(|w| w.window_phrase())
        .unwrap_or_else(|| t!("check.config.read_committed_snapshot.window.this_scan"));
    let mut summary = vec![t!("check.config.read_committed_snapshot.summary.lead")];
    let (severity, title, fix) = match r.evidence {
        RcsiEvidence::ConfigurationOnly => {
            summary.push(if r.read_waits_qualified {
                t!("check.config.read_committed_snapshot.summary.no_read_waited", window = &window)
            } else {
                t!("check.config.read_committed_snapshot.summary.waits_unknown")
            });
            summary.push(t!("check.config.read_committed_snapshot.summary.matters_more"));
            summary.push(t!("check.config.read_committed_snapshot.summary.most_apps"));
            (
                Severity::Low,
                t!("check.config.read_committed_snapshot.title.advice"),
                Fix {
                    summary: t!("check.config.read_committed_snapshot.fix"),
                    sql: Some(format!("ALTER DATABASE {db} SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT;")),
                    cost: Some(if r.snapshot_isolation {
                        t!("check.config.read_committed_snapshot.fix_cost.versions_kept")
                    } else {
                        t!("check.config.read_committed_snapshot.fix_cost.tempdb")
                    }),
                },
            )
        }
        RcsiEvidence::AggregateReadLocks { severity } => {
            let (which, statement, statement_wait) = match r.concentration {
                ReadLockConcentration::NearlyAll(query) => ("nearly_all", query_handle(query), String::new()),
                ReadLockConcentration::Most { query, milliseconds } => {
                    ("most", query_handle(query), format::duration(milliseconds))
                }
                ReadLockConcentration::Distributed => ("none", String::new(), String::new()),
            };
            let came = match (&r.waiting.when, r.waiting.stopped) {
                (Some(_), true) => "when_stopped",
                (Some(_), false) => "when",
                (None, true) => "stopped",
                (None, false) => "none",
            };
            summary.push(t!(
                "check.config.read_committed_snapshot.summary.observed",
                window = &window,
                duration = format::duration(r.lock_ms),
                share = format::percent(r.share),
                which = which,
                statement = statement,
                statement_wait = statement_wait,
                amount = if r.per_day.is_some() { "per_day" } else { came },
                per_day = r.per_day.map(format::duration).unwrap_or_default(),
                when = r.waiting.when.clone().unwrap_or_default(),
                reads = r.waiting.reads.len()
            ));
            summary.push(t!("check.config.read_committed_snapshot.summary.cause_unknown"));
            (
                severity,
                t!("check.config.read_committed_snapshot.title.candidate"),
                Fix { summary: t!("check.config.read_committed_snapshot.fix.investigate"), sql: None, cost: None },
            )
        }
    };
    let mut finding = setting_finding("read-committed-snapshot", severity, title, summary.join(" "), fix);
    if let RcsiEvidence::AggregateReadLocks { .. } = r.evidence {
        finding.metrics.push(Metric::said(
            culprit_core::key!("check.config.read_committed_snapshot.metric.reads_waiting"),
            r.lock_ms,
            Unit::Ms,
        ));
        finding.verify = vec![t!("check.config.read_committed_snapshot.verify.investigate")];
    }
    finding.caveats.push(t!("check.config.read_committed_snapshot.caveat.readers"));
    if r.snapshot_isolation {
        finding.caveats.push(t!("check.config.read_committed_snapshot.caveat.snapshot_isolation"));
    }
    finding
}

/// The newest compatibility level the server offers. Azure SQL Database and Managed Instance report a product version
/// of 12.0 whatever they run, so their level comes from the service, not the version: 160 on both today.
fn latest_compat(server: &ServerInfo) -> u32 {
    if server.platform().is_some_and(|p| p.is_azure_sql()) {
        return 160;
    }
    let major: u32 = server.version.split('.').next().and_then(|v| v.parse().ok()).unwrap_or(16);
    (major * 10).clamp(100, 170)
}

/// The findings this file makes of `snapshot`.
#[cfg(test)]
pub(super) fn check(snapshot: &Snapshot) -> Vec<Finding> {
    decide(snapshot).into_iter().map(|case| render(snapshot, case)).collect()
}
