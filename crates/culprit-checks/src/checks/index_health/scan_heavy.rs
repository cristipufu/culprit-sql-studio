//! The `scan-heavy` check: Large tables that queries read in full many times a day, from the table scan counters.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn scan_heavy_info() -> CheckInfo {
    CheckInfo {
        id: "scan-heavy".into(),
        title: t!("check.scan_heavy.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Postgres, Engine::Mysql],
        requires: vec![Capability::TableStats],
        description: t!("check.scan_heavy.info.description"),
    }
}

check!(
    ScanHeavyCheck: scan_heavy_info,
    decide = scan_heavy_tables,
    render = scan_heavy_finding,
    case<'s> = ScanHeavy<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::Scans)],
);

/// Full-table reads and rows read without an index since the counters started, and the days that covers. A full read
/// is the table's rows read once: PostgreSQL's `seq_scan` counts a scan once for every process that runs it (each
/// parallel worker and the leader) and again on every rescan, and counts a scan a LIMIT stopped early as a whole one,
/// so the rows it read, divided by the table's rows, is the count to say when it is lower.
pub(crate) fn full_scans_measured(snapshot: &Snapshot, t: &culprit_core::TableStat) -> Option<(f64, f64, f64)> {
    let days = counter_days_for_rate(snapshot)?;
    let rows = t.rows.filter(|r| *r >= 50_000.0)?;
    let scanned = t.seq_rows_read?;
    let full_reads = (scanned / rows).round();
    let scans = t.seq_scans.map_or(full_reads, |n| n.min(full_reads));
    Some((scans, scanned, days))
}

/// A statement with no WHERE: a GROUP BY or a report over the whole table, an UPDATE of every row. It reads the table
/// in full by what it asks for; its sort, grouping or result size is another finding's.
fn reads_every_row(q: &QueryStat) -> bool {
    !mentions_word(&q.text, "where")
}

/// The captured statements that read a table in full: their plan scans it, or, without a plan, they read or change rows
/// of it (SELECT, UPDATE, DELETE, MERGE) and name it. A statement that seeks the table through an index, or only
/// inserts into it, isn't behind its full reads, so its time isn't theirs. The check sets apart those with no WHERE
/// ([`reads_every_row`]), which read every row on purpose, and lists the rest; the index advisor lists what it lists.
fn reading_in_full<'a>(snapshot: &'a Snapshot, schema: &str, table: &str) -> Vec<&'a QueryStat> {
    let qualified = format!("{schema}.{table}").to_ascii_lowercase();
    let names_table = |object: &str| {
        let o = object.to_ascii_lowercase().replace(['[', ']', '"', '`'], "");
        o == qualified || o.ends_with(&format!(".{qualified}")) || o == table.to_ascii_lowercase()
    };
    queries_touching(snapshot, schema, table)
        .into_iter()
        .filter(|q| !is_monitoring_query(q))
        .filter(|q| match &q.plan {
            Some(plan) => plan
                .nodes()
                .iter()
                .any(|n| n.kind == culprit_core::NodeKind::Scan && n.object.as_deref().is_some_and(names_table)),
            None => {
                let verb = q.text.trim_start_matches(|c: char| c.is_whitespace() || c == '(').to_ascii_lowercase();
                ["select", "with", "update", "delete", "merge"].iter().any(|v| verb.starts_with(v))
            }
        })
        .collect()
}

/// What a statement's runs say about reading a table of `table_rows` rows in full, where the engine counts the rows
/// each statement examined (MySQL and MariaDB): under a tenth of the table a run, it reaches the rows through an index
/// (`Some(false)`); half of it or more a run, from a statement that names no other table, it reads it in full
/// (`Some(true)`). `None` where the counts can't say: another engine, no count, a join (its rows examined are every
/// table's), or anything in between.
fn examined_reads_whole(snapshot: &Snapshot, q: &QueryStat, table_rows: f64) -> Option<bool> {
    if snapshot.server.engine != Engine::Mysql || table_rows <= 0.0 {
        return None;
    }
    let examined = q.reads_per_execution()?;
    if examined < table_rows * 0.1 {
        return Some(false);
    }
    let one_table = culprit_core::sql::tables_in(&q.text, Engine::Mysql).len() == 1;
    (examined >= table_rows * 0.5 && one_table).then_some(true)
}

/// The sentence for statements with no WHERE that read the table in full: on purpose, and nothing an index on a
/// filter changes. They aren't listed with the finding (its fix is about filters), so it names them.
fn on_purpose(whole: &[&QueryStat]) -> Option<String> {
    let ids: Vec<String> = whole.iter().map(|q| q.id.clone()).collect();
    match whole {
        [] => None,
        [one] => {
            let handle = query_handle(one);
            let mut chars = handle.chars();
            let handle =
                chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            Some(t!("check.scan_heavy.summary.whole_one", handle = handle, id = &ids[0]))
        }
        many => {
            Some(t!("check.scan_heavy.summary.whole_many", count = many.len().to_string(), ids = format::list(&ids)))
        }
    }
}

/// A large table read in full over and over, and what its finding concludes.
pub struct ScanHeavy<'s> {
    table: &'s culprit_core::TableStat,
    /// Full reads, and rows read without an index, since the counters started, over these days.
    full_reads: f64,
    rows_read: f64,
    days: f64,
    /// Said a day (over 20 hours of counters), otherwise as counted.
    measured: bool,
    /// The statements listed with it all stopped running: told in the past.
    stopped: bool,
    /// The captured statements shown reading it in full, and how they show it.
    proven: Vec<&'s QueryStat>,
    shown: Option<Shown>,
    /// Statements without a captured plan that may read it in full.
    unplanned: Vec<&'s QueryStat>,
    /// Statements with no WHERE: they read it in full on purpose.
    whole: Vec<&'s QueryStat>,
    /// Some captured statement names it (and reaches it through an index).
    named: bool,
    /// The share of database time of the statements behind the full reads, where it comes to something.
    impact: Option<QueryImpact>,
    severity: Severity,
    /// Its score without such a share.
    score: f64,
    confidence: Confidence,
}

/// How the captured statements show they read a table in full.
enum Shown {
    /// One plan scans it.
    Plan,
    /// Their plans scan it.
    Plans,
    /// One statement examines about the whole table a run: these rows.
    Rows(f64),
    /// Each examines half its rows or more a run.
    ManyRows,
    /// By their plans, or by the rows they examine.
    Both,
}

fn scan_heavy_tables(snapshot: &Snapshot) -> Vec<ScanHeavy<'_>> {
    let planned: Vec<(String, String)> = scan_groups(snapshot)
        .into_iter()
        .map(|g| (g.schema.to_ascii_lowercase(), g.table.to_ascii_lowercase()))
        .collect();
    let mut out = Vec::new();
    // A statement that reads every partition reads each one in full: judged per partition it would be one finding per
    // partition for one cause.
    for t in snapshot.tables.iter().filter(|t| t.partition_of.is_none()) {
        let Some((scans_total, read_total, days)) = full_scans_measured(snapshot, t) else { continue };
        // Judged per day; said per day only over 20 hours of counters, otherwise as counted.
        let (scans, rows_read) = (scans_total / days, read_total / days);
        let measured = counters_measure_a_day(snapshot, days);
        if scans < 50.0 || rows_read < 5_000_000.0 {
            continue;
        }
        // A captured plan already names the filter and the index to add: the filtered-scan finding says it better.
        if planned.contains(&(t.schema.to_ascii_lowercase(), t.name.to_ascii_lowercase())) {
            continue;
        }
        let touching: Vec<&QueryStat> =
            queries_touching(snapshot, &t.schema, &t.name).into_iter().filter(|q| !is_monitoring_query(q)).collect();
        let (whole, scanning): (Vec<&QueryStat>, Vec<&QueryStat>) =
            reading_in_full(snapshot, &t.schema, &t.name).into_iter().partition(|q| reads_every_row(q));
        // A plan that scans it shows the full read, and so do the rows a statement examined on MySQL when they are about
        // the whole table. A statement without either may as well seek it (most reads by key do): listed to explain,
        // but its time isn't counted as the scans' cost; and one whose rows examined are a sliver of the table reaches
        // them through an index, so it isn't listed at all.
        let table_rows = t.rows.unwrap_or(0.0);
        let (by_plan, rest): (Vec<&QueryStat>, Vec<&QueryStat>) = scanning.iter().partition(|q| q.plan.is_some());
        let (by_rows, unplanned): (Vec<&QueryStat>, Vec<&QueryStat>) = rest
            .into_iter()
            .filter(|q| examined_reads_whole(snapshot, q, table_rows) != Some(false))
            .partition(|q| examined_reads_whole(snapshot, q, table_rows) == Some(true));
        let proven: Vec<&QueryStat> = by_plan.iter().chain(&by_rows).copied().collect();
        // How the proven ones show it: their plans, the rows they examine, or both.
        let shown = (!proven.is_empty()).then(|| match (by_plan.is_empty(), by_rows.as_slice()) {
            (false, []) if proven.len() == 1 => Shown::Plan,
            (false, []) => Shown::Plans,
            (true, [q]) => Shown::Rows(q.reads_per_execution().unwrap_or(0.0)),
            (true, _) => Shown::ManyRows,
            (false, _) => Shown::Both,
        });
        // Statements with no WHERE are behind full reads too, but on purpose. When the query history has nothing else
        // that reads it in full and their plans read it whole as often as it was read in full, the reads are theirs, and
        // no index on a filter has anything to remove.
        let whole_runs: f64 = whole.iter().filter(|q| q.plan.is_some()).map(|q| q.executions).sum();
        if scanning.is_empty() && !whole.is_empty() && whole_runs >= scans_total {
            continue;
        }
        // Read in full by statements that all stopped running: told in the past, as the finding then is.
        let listed: Vec<&QueryStat> = proven.iter().chain(&unplanned).copied().take(5).collect();
        // Only the statements whose plans read it in full are behind the scans, and their time is the cost.
        let impact = query_impact(snapshot, &proven).filter(|i| i.share >= 0.005);
        let severity = match &impact {
            // Their share and the amount it comes to, on one basis (Culprit's scans where they measured them).
            Some(i) => severity_for_statement_share(snapshot, &proven, i.share),
            None if rows_read >= 1e9 => Severity::Medium,
            None => Severity::Low,
        };
        out.push(ScanHeavy {
            table: t,
            full_reads: scans_total,
            rows_read: read_total,
            days,
            measured,
            stopped: crate::all_stopped(snapshot, &listed),
            confidence: if proven.is_empty() { Confidence::Low } else { Confidence::Medium },
            shown,
            proven,
            unplanned,
            whole,
            named: !touching.is_empty(),
            impact,
            severity,
            score: base_score(severity) + (rows_read.log10() - 6.0).max(0.0) * 3.0,
        });
    }
    out
}

fn scan_heavy_finding(snapshot: &Snapshot, s: ScanHeavy<'_>) -> Finding {
    let t = s.table;
    let table_name = t.qualified();
    let (scans, rows_read) = (s.full_reads / s.days, s.rows_read / s.days);
    let mut f = finding("scan-heavy", &table_name, Category::Indexes, subject(SubjectKind::Table, &table_name));
    let how_often = how_many(snapshot, s.full_reads, s.days, culprit_core::key!("common.noun.time"));
    let tense = if s.stopped { "past" } else { "present" };
    f.title = if s.measured {
        t!(
            "check.scan_heavy.title.per_day",
            table = &t.name,
            tense = tense,
            how_many = &how_often,
            rows_read = format::count(rows_read)
        )
    } else {
        t!(
            "check.scan_heavy.title.counted",
            table = &t.name,
            tense = tense,
            how_many = &how_often,
            rows_read = format::count(s.rows_read)
        )
    };
    let read = if s.measured {
        t!(
            "check.scan_heavy.summary.read_per_day",
            table = &table_name,
            rows = format::count(t.rows.unwrap_or(0.0)),
            how_many = &how_often,
            since = since_phrase(snapshot),
            rows_read = format::count(rows_read)
        )
    } else {
        t!(
            "check.scan_heavy.summary.read",
            table = &table_name,
            rows = format::count(t.rows.unwrap_or(0.0)),
            how_many = &how_often,
            rows_read = format::count(s.rows_read)
        )
    };
    let mut summary = vec![read];
    let on_purpose = on_purpose(&s.whole);
    if let Some(shown) = &s.shown {
        let shown = match shown {
            Shown::Plan => t!("check.scan_heavy.shown.plan"),
            Shown::Plans => t!("check.scan_heavy.shown.plans"),
            Shown::Rows(rows) => t!("check.scan_heavy.shown.rows", rows = format::count(*rows)),
            Shown::ManyRows => t!("check.scan_heavy.shown.many_rows"),
            Shown::Both => t!("check.scan_heavy.shown.both"),
        };
        let proven = s.proven.len().to_string();
        summary.push(if s.unplanned.is_empty() {
            t!("check.scan_heavy.summary.proven", proven = proven, shown = shown)
        } else {
            t!(
                "check.scan_heavy.summary.proven_and_unplanned",
                proven = proven,
                shown = shown,
                unplanned = s.unplanned.len().to_string()
            )
        });
        summary.extend(on_purpose);
    } else if !s.unplanned.is_empty() {
        summary.push(t!("check.scan_heavy.summary.unplanned", unplanned = s.unplanned.len().to_string()));
        summary.extend(on_purpose);
    } else if let Some(on_purpose) = on_purpose {
        summary.push(on_purpose);
        summary.push(t!("check.scan_heavy.summary.rest_elsewhere"));
    } else if s.named {
        summary.push(t!("check.scan_heavy.summary.through_index"));
    } else {
        summary.push(t!("check.scan_heavy.summary.unnamed"));
    }
    f.summary = summary.join(" ");
    match &s.impact {
        Some(i) => apply_query_impact(&mut f, i),
        None => {
            f.impact.score = s.score;
            f.impact.label = if s.measured {
                t!("check.scan_heavy.impact.rows_per_day", rows = format::count(rows_read))
            } else {
                t!("check.scan_heavy.impact.rows", rows = format::count(s.rows_read))
            };
        }
    }
    f.severity = s.severity;
    f.confidence = s.confidence;
    f.queries = query_refs(&s.proven.iter().chain(&s.unplanned).copied().take(5).collect::<Vec<_>>());
    if s.measured {
        f.metrics.push(Metric::new(t!("check.scan_heavy.metric.full_reads_per_day"), scans, Unit::Count));
        f.metrics.push(Metric::new(t!("check.scan_heavy.metric.rows_read_per_day"), rows_read, Unit::Rows));
    } else {
        f.metrics.push(Metric::new(t!("check.scan_heavy.metric.full_reads"), s.full_reads, Unit::Count));
        f.metrics.push(Metric::new(t!("check.scan_heavy.metric.rows_read"), s.rows_read, Unit::Rows));
    }
    if let Some(rows) = t.rows {
        f.metrics.push(Metric::new(t!("common.metric.rows_in_table"), rows, Unit::Rows));
    }
    let generic = if snapshot.server.engine == Engine::Postgres { "yes" } else { "no" };
    let explain = if f.queries.is_empty() {
        t!("check.scan_heavy.fix.unlisted", generic = generic)
    } else {
        t!("check.scan_heavy.fix.listed", generic = generic)
    };
    f.fix = Some(Fix {
        summary: [
            explain,
            t!("check.scan_heavy.fix.measure", share = format::percent(culprit_core::plan::INDEX_KEEPS_AT_MOST)),
        ]
        .join(" "),
        sql: None,
        // No SQL creates an index here: the cost line names the one the reader would add.
        cost: crate::indexes::write_cost_of(snapshot, &t.schema, &t.name, &t!("check.scan_heavy.fix_cost_index")),
    });
    // The counters keep totals since they started, so a day moves a rate over weeks by a hair: what shows the
    // change is what they grow by between two scans.
    f.verify = vec![match t.index_scans {
        Some(n) => t!(
            "check.scan_heavy.verify.counters_with_index_scans",
            table = &t.name,
            full_reads = format::count(s.full_reads),
            index_scans = format::count(n)
        ),
        None => t!("check.scan_heavy.verify.counters", table = &t.name, full_reads = format::count(s.full_reads)),
    }];
    if !f.queries.is_empty() {
        let n = f.queries.len();
        let verify = crate::queries::what_changed_between_scans(
            snapshot,
            &t!("check.scan_heavy.verify.queries", n = n),
            &t!("check.scan_heavy.verify.cheaper", n = n),
        )
        .unwrap_or_else(|| t!("check.scan_heavy.verify.rescan", n = n));
        f.verify.push(t!("check.scan_heavy.verify.either_fix", verify = verify));
    }
    f.caveats.push(t!("check.scan_heavy.caveat.counters"));
    f
}
