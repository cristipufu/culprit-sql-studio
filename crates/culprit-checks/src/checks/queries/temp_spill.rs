//! The `temp-spill` check: Statements whose sorts, hashes or temporary tables outgrow memory and spill to disk.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::t;

pub fn temp_spill_info() -> CheckInfo {
    CheckInfo {
        id: "temp-spill".into(),
        title: t!("check.temp_spill.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.temp_spill.info.description"),
    }
}

check!(
    TempSpillCheck: temp_spill_info,
    decide = temp_spills,
    render = temp_spill_finding,
    case<'s> = TempSpill<'s>,
    evidence = &[Evidence::Query(QueryMeasure::TempWrites)],
);

/// A statement whose sorts, hashes or temporary tables spill to disk, and what that establishes.
pub struct TempSpill<'s> {
    query: &'s QueryStat,
    impact: QueryImpact,
    severity: Severity,
    score: f64,
    /// It stopped running: told in the past.
    stopped: bool,
    /// The plan's step that spills, where the plan says which.
    step: Option<u32>,
    /// Its totals are counted over more than the share and the average beside them (Culprit's scans), and are said
    /// over what they are counted over.
    counted_over_totals: bool,
    spill: Spill,
}

/// How the statement spills, as each engine counts it.
enum Spill {
    /// PostgreSQL: a sort or hash outgrew work_mem and wrote temp files.
    TempFiles {
        /// Bytes written in all, and a run.
        written: f64,
        per_run: f64,
        /// work_mem as Culprit's session has it: its text, where the scan read the setting, and in bytes.
        work_mem_text: Option<String>,
        work_mem: f64,
        advice: WorkMem,
    },
    /// MySQL and MariaDB: an internal temporary table outgrew memory, or held a value the in-memory engine can't, and
    /// went to disk.
    DiskTables {
        /// Tables built on disk in all, and a run.
        tables: f64,
        per_run: f64,
        /// The share of runs that can have built one: at most every run.
        runs_share: f64,
        /// TEXT and BLOB columns the statement groups or sorts, which send its table to disk whatever its size.
        large: Vec<String>,
        limits: TempLimits,
        advice: LimitAdvice,
    },
}

/// What more work_mem can do for a spill.
enum WorkMem {
    /// It is already at the most Culprit suggests, or the spill needs no more: sort less.
    Already,
    /// This much, as the setting is written.
    Raise(String),
}

/// What raising MySQL's or MariaDB's in-memory limits can do.
enum LimitAdvice {
    /// The scan didn't read these limits' values: raising them is said without SQL that might lower one.
    Unread(Vec<&'static str>),
    /// Every limit is already above the proposal: an index is what is left.
    IndexOnly,
    /// Raise these limits to the proposal (bytes).
    Raise { proposed: f64, names: Vec<&'static str> },
}

fn temp_spills(snapshot: &Snapshot) -> Vec<TempSpill<'_>> {
    match snapshot.server.engine {
        Engine::Postgres => {}
        Engine::Mysql => return mysql_disk_temp_tables(snapshot),
        Engine::Mssql | Engine::Sqlite | Engine::Yugabyte => return Vec::new(),
    }
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    let work_mem_text = snapshot.setting("work_mem").map(|s| s.value.clone());
    let work_mem = work_mem_text.as_deref().and_then(pg_size_bytes).unwrap_or(4.0 * MB);
    let block = snapshot.setting("block_size").and_then(|s| s.value.trim().parse::<f64>().ok()).unwrap_or(8192.0);
    app_queries(snapshot)
        .filter(|q| q.temp_writes.is_some_and(|t| t > 0.0))
        .filter_map(|q| {
            let impact = query_impact(snapshot, &[q])?;
            if impact.share < 0.01 {
                return None;
            }
            let written = q.temp_writes? * block;
            let per_run = written / q.executions.max(1.0);
            // pg_stat_statements counts the blocks written, not the time: what writing them and reading them back can
            // take at a slow disk's pace, at most the run itself. A spill too small to matter to the run isn't why it's
            // slow (and mustn't stand in for the heavy-query finding that would say what is).
            let spill_ms = (per_run * 2.0 / SPILL_BYTES_PER_MS).min(q.mean_time_ms);
            if spill_ms < q.mean_time_ms * SPILL_MIN_SHARE_OF_RUN {
                return None;
            }
            let spill_share = workload.share_of(spill_ms * q.executions, q);
            Some(TempSpill {
                query: q,
                impact,
                severity: severity_for_time_of(snapshot, &[q], spill_ms * q.executions, spill_share),
                score: score_for_share(spill_share),
                stopped: crate::all_stopped(snapshot, &[q]),
                step: spilling_step(q, false),
                // The spill is kept only in the totals: where the statement's share and average are Culprit's scans',
                // what it wrote is said over what it is counted over.
                counted_over_totals: crate::support::totals_basis(snapshot, q).is_some(),
                spill: Spill::TempFiles {
                    written,
                    per_run,
                    work_mem_text: work_mem_text.clone(),
                    work_mem,
                    advice: work_mem_advice(work_mem, per_run),
                },
            })
        })
        .collect()
}

fn temp_spill_finding(snapshot: &Snapshot, spill: TempSpill<'_>) -> Finding {
    let q = spill.query;
    let mut f = finding("temp-spill", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    apply_impact(snapshot, &mut f, &spill.impact);
    f.severity = spill.severity;
    f.impact.score = spill.score;
    f.confidence = Confidence::High;
    let query = capitalize(&query_handle(q));
    let tense = if spill.stopped { "past" } else { "present" };
    let basis = crate::support::totals_basis(snapshot, q).filter(|_| spill.counted_over_totals);
    let basis = basis.as_deref();
    let runs = format::count(q.executions);
    f.queries = vec![QueryRef { id: q.id.clone(), plan_node: spill.step, note: None }];
    match spill.spill {
        Spill::TempFiles { written, per_run, work_mem_text, work_mem, advice } => {
            f.title =
                t!("check.temp_spill.title.temp_files", query = query, tense = tense, size = format::bytes(per_run));
            // work_mem as Culprit's session has it: a role or database setting (ALTER ROLE … SET) gives the
            // application's sessions their own, which the scan doesn't read.
            let work_mem_said = work_mem_text.clone().unwrap_or_else(|| t!("check.temp_spill.work_mem_default"));
            let written_said = format::bytes(written);
            f.summary = match basis {
                Some(basis) => t!(
                    "check.temp_spill.summary.temp_files_over",
                    tense = tense,
                    work_mem = work_mem_said,
                    written = written_said.clone(),
                    runs = runs.clone(),
                    basis = basis
                ),
                None => t!(
                    "check.temp_spill.summary.temp_files",
                    tense = tense,
                    work_mem = work_mem_said,
                    written = written_said.clone(),
                    runs = runs.clone()
                ),
            };
            let label = match basis {
                Some(basis) => t!("check.temp_spill.metric.temp_written_over", basis = basis),
                None => t!("check.temp_spill.metric.temp_written"),
            };
            f.metrics.push(Metric::new(label, per_run, Unit::Bytes));
            f.metrics.extend(crate::queries::average_metrics(snapshot, q));
            f.fix = Some(work_mem_fix(work_mem, work_mem_text.as_deref(), &advice));
            f.verify = vec![spill_verify(
                snapshot,
                t!("check.temp_spill.live.setting"),
                Counted::TempFiles,
                t!("check.temp_spill.now.temp_files", written = written_said, runs = runs),
            )];
        }
        Spill::DiskTables { tables, per_run, runs_share, large, limits, advice } => {
            f.title = if per_run >= 0.95 && per_run.round() <= 1.0 {
                t!("check.temp_spill.title.table_once", query = query, tense = tense)
            } else if per_run >= 0.95 {
                t!(
                    "check.temp_spill.title.tables_per_run",
                    query = query,
                    tense = tense,
                    count = format::count(per_run.round())
                )
            } else {
                t!(
                    "check.temp_spill.title.tables_some_runs",
                    query = query,
                    tense = tense,
                    share = format::percent(runs_share)
                )
            };
            let tables_said = format::count(tables);
            f.summary = match (large.is_empty(), basis) {
                (true, None) => t!(
                    "check.temp_spill.summary.tables",
                    tense = tense,
                    limit = limits.describe(),
                    tables = tables_said.clone(),
                    runs = runs.clone()
                ),
                (true, Some(basis)) => t!(
                    "check.temp_spill.summary.tables_over",
                    tense = tense,
                    limit = limits.describe(),
                    tables = tables_said.clone(),
                    runs = runs.clone(),
                    basis = basis
                ),
                (false, None) => t!(
                    "check.temp_spill.summary.large_values",
                    tense = tense,
                    columns = format::list(&large),
                    server = snapshot.server.name(),
                    tables = tables_said.clone(),
                    runs = runs.clone()
                ),
                (false, Some(basis)) => t!(
                    "check.temp_spill.summary.large_values_over",
                    tense = tense,
                    columns = format::list(&large),
                    server = snapshot.server.name(),
                    tables = tables_said.clone(),
                    runs = runs.clone(),
                    basis = basis
                ),
            };
            let live = if large.is_empty() && !limits.caps.is_empty() {
                t!("check.temp_spill.live.limits")
            } else {
                t!("check.temp_spill.live.change")
            };
            f.verify = vec![spill_verify(
                snapshot,
                live,
                Counted::DiskTables,
                t!("check.temp_spill.now.disk_tables", tables = tables_said, runs = runs),
            )];
            let label = match basis {
                Some(basis) => t!("check.temp_spill.metric.disk_tables_over", basis = basis),
                None => t!("check.temp_spill.metric.disk_tables"),
            };
            f.metrics.push(Metric::new(label, per_run, Unit::Count));
            f.metrics.extend(crate::queries::average_metrics(snapshot, q));
            f.fix = Some(if large.is_empty() {
                limits.fix(snapshot, &advice)
            } else {
                Fix {
                    summary: t!("check.temp_spill.fix.large_values", columns = format::list(&large)),
                    sql: None,
                    cost: None,
                }
            });
        }
    }
    f
}

/// What a later scan compares of a spill: its temp files, or its on-disk temporary tables.
#[derive(Clone, Copy)]
enum Counted {
    TempFiles,
    DiskTables,
}

/// How a later scan shows the spill gone. A windowed source drops the runs from before the change once its window has
/// passed them. A source of running totals keeps them, and this finding with them: what shows it is the statement's
/// count staying as it is while its runs go on, between a scan taken when the change is live and a later one, said
/// with this scan's numbers (`now`) ([`verify_counts_between_scans`]).
fn spill_verify(snapshot: &Snapshot, live: String, counted: Counted, now: String) -> String {
    let what = match counted {
        Counted::TempFiles => t!("check.temp_spill.counted.temp_files"),
        Counted::DiskTables => t!("check.temp_spill.counted.disk_tables"),
    };
    match (verify_counts_between_scans(snapshot, &what, &now), counted) {
        (Some(compare), Counted::TempFiles) => {
            t!("check.temp_spill.verify.temp_files", live = live, compare = compare)
        }
        (Some(compare), Counted::DiskTables) => {
            t!("check.temp_spill.verify.disk_tables", live = live, compare = compare)
        }
        (None, _) => t!("check.temp_spill.verify.windowed", live = live),
    }
}

/// The plan's step that spills, to point at: the one the plan saw spill, else the only one that can. PostgreSQL's temp
/// files come from a sort, a hash join's hash or a hash aggregate; MySQL's on-disk tables from an internal temporary
/// table (`temp_tables`), which no plan marks as spilled (MariaDB's spill mark is a sort's, not the table's). None when
/// the plan shows no such step, or several and nothing says which.
fn spilling_step(q: &QueryStat, temp_tables: bool) -> Option<u32> {
    let nodes = q.plan.as_ref()?.nodes();
    if !temp_tables && let Some(n) = nodes.iter().find(|n| n.warnings.contains(&PlanWarning::Spill)) {
        return Some(n.id);
    }
    let can_spill = |n: &&&PlanNode| {
        if temp_tables {
            n.op == "Temporary table" || n.detail.as_deref().is_some_and(|d| d.contains("temporary table"))
        } else {
            n.kind == NodeKind::Sort
                || n.op == "Hash"
                || n.facts().aggregate_strategy.as_ref().is_some_and(|s| s == "Hashed" || s == "Mixed")
        }
    };
    let mut steps = nodes.iter().filter(can_spill);
    match (steps.next(), steps.next()) {
        (Some(only), None) => Some(only.id),
        _ => None,
    }
}

pub(super) const MB: f64 = 1024.0 * 1024.0;
/// A slow disk's pace for temp files (100 MB/s), in bytes per millisecond.
const SPILL_BYTES_PER_MS: f64 = 100.0 * MB / 1000.0;
/// A spill that can take less than this share of a run isn't what makes the statement slow.
const SPILL_MIN_SHARE_OF_RUN: f64 = 0.1;
/// The most sort memory Culprit suggests for one role: past it, the fix is to sort less.
const WORK_MEM_MAX: f64 = 1024.0 * MB;

/// More work_mem than the spill needed, for the application's role or one transaction: never less than it has now, and
/// never past [`WORK_MEM_MAX`].
fn work_mem_advice(current: f64, per_run: f64) -> WorkMem {
    let wanted = (current + per_run * 2.0).max(16.0 * MB);
    let proposed = 2f64.powf(wanted.log2().ceil());
    if proposed > WORK_MEM_MAX || proposed <= current {
        return WorkMem::Already;
    }
    WorkMem::Raise(if proposed >= 1024.0 * MB {
        format!("{:.0}GB", proposed / (1024.0 * MB))
    } else {
        format!("{:.0}MB", proposed / MB)
    })
}

/// The fix [`work_mem_advice`] says, for the application's role or one transaction.
fn work_mem_fix(current: f64, current_text: Option<&str>, advice: &WorkMem) -> Fix {
    let now = current_text.map(str::to_string).unwrap_or_else(|| format::bytes(current));
    match advice {
        WorkMem::Already => {
            Fix { summary: t!("check.temp_spill.fix.work_mem_already", now = now), sql: None, cost: None }
        }
        WorkMem::Raise(value) => Fix {
            summary: t!("check.temp_spill.fix.work_mem", value = value, now = now),
            sql: Some(format!(
                "-- For the role the application connects as (takes effect on its next connections):\nALTER ROLE <app_role> SET work_mem = '{value}';\n\n-- Or only for this statement, inside its transaction:\nSET LOCAL work_mem = '{value}';"
            )),
            cost: Some(t!("check.temp_spill.fix.cost.work_mem")),
        },
    }
}

/// A PostgreSQL size setting ("4MB", "64kB", "1GB", or a bare number of kB) in bytes.
pub(super) fn pg_size_bytes(text: &str) -> Option<f64> {
    let t = text.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let number: f64 = t[..split].parse().ok()?;
    let unit = match t[split..].trim().to_ascii_lowercase().as_str() {
        "" | "kb" => 1024.0,
        "b" => 1.0,
        "mb" => MB,
        "gb" => 1024.0 * MB,
        "tb" => 1024.0 * 1024.0 * MB,
        _ => return None,
    };
    Some(number * unit)
}

/// MySQL counts on-disk temporary tables per digest: a GROUP BY, DISTINCT or ORDER BY whose temporary table outgrew
/// memory, or held a value the in-memory engine can't (TEXT and BLOB, before MySQL 8.0's TempTable engine).
fn mysql_disk_temp_tables(snapshot: &Snapshot) -> Vec<TempSpill<'_>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    let limits = TempLimits::of(snapshot);
    app_queries(snapshot)
        .filter(|q| q.temp_writes.is_some_and(|t| t > 0.0))
        .filter_map(|q| {
            let impact = query_impact(snapshot, &[q])?;
            if impact.share < 0.01 {
                return None;
            }
            let tables = q.temp_writes?;
            let per_run = tables / q.executions.max(1.0);
            // MySQL counts tables, not runs: a run can build more than one, so the runs that spilled are at most this.
            let runs_share = per_run.min(1.0);
            let spilled_share = workload.share_of(q.total_time_ms * runs_share, q);
            let large = if limits.blob_bound { large_value_columns(snapshot, q) } else { Vec::new() };
            // The time of the runs that can have spilled, not the whole statement's. A table that outgrew the memory
            // limit is at least that big, work that costs. One sent to disk for a TEXT or BLOB value can be a few
            // rows, cheap where the file stays in the operating system's cache: nothing says it is what the run
            // spends its time on, so it is Low.
            let severity = if large.is_empty() {
                severity_for_time_of(snapshot, &[q], q.total_time_ms * runs_share, spilled_share)
            } else {
                Severity::Low
            };
            Some(TempSpill {
                query: q,
                impact,
                severity,
                score: score_for_share(if large.is_empty() { spilled_share } else { spilled_share * 0.25 }),
                stopped: crate::all_stopped(snapshot, &[q]),
                step: spilling_step(q, true),
                // Counted only in the totals: said over what they are counted over where the share is Culprit's scans'.
                counted_over_totals: crate::support::totals_basis(snapshot, q).is_some(),
                spill: Spill::DiskTables {
                    tables,
                    per_run,
                    runs_share,
                    large,
                    advice: limits.advice(),
                    limits: limits.clone(),
                },
            })
        })
        .collect()
}

/// What keeps a MySQL or MariaDB internal temporary table in memory, as this server is set up.
#[derive(Clone)]
pub(super) struct TempLimits {
    /// The variables that cap one table in memory, with their values (bytes) where the scan read them.
    caps: Vec<(&'static str, Option<f64>)>,
    /// MySQL 8.0's TempTable engine also stops at temptable_max_ram, for all its tables together: its value where the
    /// scan read it.
    shared_ram: Option<Option<f64>>,
    /// A TEXT or BLOB value sends the table to disk whatever its size (the MEMORY engine: MariaDB, MySQL 5.7).
    blob_bound: bool,
    /// MySQL 8.0 keeps these tables in TempTable or MEMORY (`internal_tmp_mem_storage_engine`), and the scan didn't
    /// read which: both variables of MEMORY's cap are raised, and nothing is said of TempTable's limit but its name.
    engine_unknown: bool,
}

impl TempLimits {
    fn of(snapshot: &Snapshot) -> TempLimits {
        let value = |name: &str| snapshot.setting(name).and_then(|s| s.value.trim().parse::<f64>().ok());
        let version = &snapshot.server.version;
        let mysql8 = snapshot.server.name() == "MySQL" && culprit_core::version_at_least(version, [8, 0, 0]);
        let engine = snapshot.setting("internal_tmp_mem_storage_engine").map(|s| s.value.trim().to_ascii_lowercase());
        let both =
            vec![("tmp_table_size", value("tmp_table_size")), ("max_heap_table_size", value("max_heap_table_size"))];
        if mysql8 && engine.as_deref() == Some("temptable") {
            // TempTable ignores max_heap_table_size; tmp_table_size caps each table from 8.0.28.
            let caps = if culprit_core::version_at_least(version, [8, 0, 28]) {
                vec![("tmp_table_size", value("tmp_table_size"))]
            } else {
                Vec::new()
            };
            return TempLimits {
                caps,
                shared_ram: Some(value("temptable_max_ram")),
                blob_bound: false,
                engine_unknown: false,
            };
        }
        if mysql8 && engine.is_none() {
            return TempLimits { caps: both, shared_ram: None, blob_bound: false, engine_unknown: true };
        }
        TempLimits {
            caps: both,
            shared_ram: None,
            // MySQL 8.0's MEMORY engine, MySQL 5.7 and MariaDB can't hold TEXT or BLOB values in memory.
            blob_bound: true,
            engine_unknown: false,
        }
    }

    /// The binding in-memory limit, in words: "the in-memory limit (1 MB, the smaller of tmp_table_size and max_heap_table_size)".
    fn describe(&self) -> String {
        let known: Vec<f64> = self.caps.iter().filter_map(|(_, v)| *v).collect();
        let smallest = known.iter().copied().reduce(f64::min);
        let names =
            |caps: &[(&str, Option<f64>)]| format::list(&caps.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>());
        match (self.caps.as_slice(), smallest, &self.shared_ram) {
            ([(name, Some(v))], _, Some(Some(ram))) => t!(
                "check.temp_spill.limit.cap_and_ram",
                name = *name,
                size = format::bytes(*v),
                ram = format::bytes(*ram)
            ),
            ([(name, Some(v))], _, Some(None)) => {
                t!("check.temp_spill.limit.cap_and_unread_ram", name = *name, size = format::bytes(*v))
            }
            ([], _, Some(Some(v))) => t!("check.temp_spill.limit.ram", size = format::bytes(*v)),
            ([], _, Some(None)) => t!("check.temp_spill.limit.unread_ram"),
            (caps, Some(v), _) if caps.len() > 1 && self.engine_unknown => {
                t!("check.temp_spill.limit.smaller_of_memory", size = format::bytes(v), names = names(caps))
            }
            (caps, Some(v), _) if caps.len() > 1 => {
                t!("check.temp_spill.limit.smaller_of", size = format::bytes(v), names = names(caps))
            }
            _ => t!("check.temp_spill.limit.unknown"),
        }
    }

    /// Raise every cap below the proposal to it, through the server's own way of changing settings; never lower one.
    fn advice(&self) -> LimitAdvice {
        let (names, current): (Vec<&'static str>, Option<f64>) = if self.caps.is_empty() {
            (vec!["temptable_max_ram"], self.shared_ram.flatten())
        } else {
            (self.caps.iter().map(|(n, _)| *n).collect(), self.caps.iter().filter_map(|(_, v)| *v).reduce(f64::min))
        };
        let floor = if self.caps.is_empty() { 2048.0 * MB } else { 64.0 * MB };
        let proposed = current.map_or(floor, |c| (c * 2.0).max(floor));
        // A value the scan didn't read could be higher than the proposal: no SQL that might lower it.
        if names.iter().any(|n| self.now(n).is_none()) {
            return LimitAdvice::Unread(names);
        }
        let names: Vec<&'static str> =
            names.into_iter().filter(|name| self.now(name).is_some_and(|v| v < proposed)).collect();
        if names.is_empty() { LimitAdvice::IndexOnly } else { LimitAdvice::Raise { proposed, names } }
    }

    /// A limit's value, where the scan read it.
    fn now(&self, name: &str) -> Option<f64> {
        if self.caps.is_empty() {
            self.shared_ram.flatten()
        } else {
            self.caps.iter().find(|(n, _)| *n == name).and_then(|(_, v)| *v)
        }
    }

    /// The fix `advice` says: an index first, then the limits raised as this server changes settings.
    fn fix(&self, snapshot: &Snapshot, advice: &LimitAdvice) -> Fix {
        // The per-table caps are session variables on both MySQL and MariaDB; the shared TempTable RAM cap isn't.
        let new_connections = (!self.caps.is_empty()).then(|| t!("check.temp_spill.fix.new_connections"));
        match advice {
            LimitAdvice::Unread(names) => {
                let mut summary = vec![t!(
                    "check.temp_spill.fix.unread",
                    names = format::list(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>()),
                    count = names.len()
                )];
                summary.extend(new_connections);
                Fix { summary: summary.join(" "), sql: None, cost: None }
            }
            LimitAdvice::IndexOnly => Fix { summary: t!("check.temp_spill.fix.index"), sql: None, cost: None },
            LimitAdvice::Raise { proposed, names } => {
                let changes: Vec<SettingChange> = names
                    .iter()
                    .map(|name| setting_change(&snapshot.server, name, &format!("{proposed:.0}")))
                    .collect();
                let sql: Vec<String> = changes.iter().filter_map(|c| c.sql.clone()).collect();
                let how: Vec<String> = changes.iter().map(|c| c.how.clone()).collect();
                let size = format::bytes(*proposed);
                let mut summary = vec![
                    match self.caps.len() {
                        0 | 1 => t!("check.temp_spill.fix.raise", size = size),
                        _ if self.engine_unknown => t!("check.temp_spill.fix.raise_both_memory", size = size),
                        _ => t!("check.temp_spill.fix.raise_both", size = size),
                    },
                    how.join(" "),
                ];
                summary.extend(new_connections);
                Fix {
                    summary: summary.join(" "),
                    sql: (!sql.is_empty()).then(|| sql.join("\n")),
                    cost: Some(if self.caps.is_empty() {
                        t!("check.temp_spill.fix.cost.shared_ram")
                    } else {
                        t!("check.temp_spill.fix.cost.per_table")
                    }),
                }
            }
        }
    }
}

/// TEXT and BLOB columns of the statement's tables that its text names (or all of them, for `SELECT *`).
pub(super) fn large_value_columns(snapshot: &Snapshot, q: &QueryStat) -> Vec<String> {
    let text = squeeze(&q.text);
    let star = text.contains("select *") || text.contains(".*");
    let mut out: Vec<String> = Vec::new();
    for c in &snapshot.columns {
        let t = c.data_type.to_ascii_lowercase();
        let large = t.contains("text") || t.contains("blob") || t == "json" || t == "longtext";
        if large
            && mentions_word(&text, &c.table.to_ascii_lowercase())
            && (star || mentions_word(&text, &c.name.to_ascii_lowercase()))
            && !out.contains(&c.name)
        {
            out.push(c.name.clone());
        }
    }
    out
}

#[cfg(test)]
pub(super) mod wording_tests {
    use super::*;
    use culprit_core::{NodeKind, Properties};

    #[test]
    fn plan_steps_take_the_right_article() {
        let mut n = PlanNode {
            facts: Default::default(),
            id: 1,
            op: "Index Scan".into(),
            kind: NodeKind::Scan,
            detail: None,
            object: Some("orders".into()),
            index: None,
            seek_predicate: None,
            predicate: None,
            est_rows: 1.0,
            est_executions: 1.0,
            act_rows: None,
            act_executions: None,
            rows_read: None,
            table_rows: None,
            subtree_cost: 1.0,
            self_cost: 1.0,
            act_time_ms: None,
            warnings: vec![],
            props: Properties::new(),
            children: vec![],
            runtime: None,
        };
        let plan = |n: &PlanNode| Plan {
            facts: Default::default(),
            view: None,
            engine: Engine::Postgres,
            kind: culprit_core::PlanKind::Estimated,
            statement: None,
            total_cost: 1.0,
            root: n.clone(),
            warnings: vec![],
            index_hints: vec![],
            props: Properties::new(),
            raw: None,
            insights: None,
        };
        assert_eq!(describe_node(&plan(&n), &n, None), "an index scan of orders");
        n.op = "Seq Scan".into();
        assert_eq!(describe_node(&plan(&n), &n, None), "a seq scan of orders");
    }

    #[test]
    fn rows_a_step_reads_are_said_as_measured_or_as_estimated() {
        let mut n = PlanNode {
            facts: Default::default(),
            id: 1,
            op: "Index range scan".into(),
            kind: NodeKind::Seek,
            detail: None,
            object: Some("shop.orders".into()),
            index: Some("ix_orders_status_created".into()),
            seek_predicate: None,
            predicate: None,
            est_rows: 29_126.0,
            est_executions: 1.0,
            act_rows: None,
            act_executions: None,
            rows_read: Some(29_126.0),
            table_rows: None,
            subtree_cost: 1.0,
            self_cost: 1.0,
            act_time_ms: None,
            warnings: vec![],
            props: Properties::new(),
            children: vec![],
            runtime: None,
        };
        let plan = |n: &PlanNode| Plan {
            facts: Default::default(),
            view: None,
            engine: Engine::Mysql,
            kind: culprit_core::PlanKind::Estimated,
            statement: None,
            total_cost: 1.0,
            root: n.clone(),
            warnings: vec![],
            index_hints: vec![],
            props: Properties::new(),
            raw: None,
            insights: None,
        };
        // An estimated plan's rows read are an estimate, not what the runs read.
        assert_eq!(
            describe_node(&plan(&n), &n, None),
            "an index range scan of shop.orders, estimated to read 29K rows"
        );
        // MySQL's rows examined per run are this step's when it reads the plan's only table.
        assert_eq!(
            describe_node(&plan(&n), &n, Some(10_715.0)),
            "an index range scan of shop.orders that examines 11K rows a run"
        );
        // Measured by the plan itself.
        n.act_rows = Some(20_000.0);
        n.rows_read = Some(20_000.0);
        assert_eq!(describe_node(&plan(&n), &n, None), "an index range scan of shop.orders that reads 20K rows");
    }
}
