//! The `large-rows` check: Tables whose rows average many kilobytes, most of it in large values kept apart from the rows (SQL Server's LOB pages, PostgreSQL's TOAST): every statement that reads whole rows pulls them in, every write that sets them writes them again. From the catalog's page counts; no data is read.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn large_rows_info() -> CheckInfo {
    CheckInfo {
        id: "large-rows".into(),
        title: t!("check.large_rows.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Mssql, Engine::Postgres],
        requires: vec![],
        description: t!("check.large_rows.info.description"),
    }
}

check!(
    LargeRowsCheck: large_rows_info,
    decide = large_rows,
    render = large_rows_finding,
    case<'s> = LargeRows<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::RowSize)],
);

/// A row this big on average is mostly a payload, not a record.
pub(super) const LARGE_ROW_BYTES: f64 = 16.0 * 1024.0;
/// Below this the large values cost too little to be worth a finding.
const LARGE_ROWS_TABLE_BYTES: f64 = 256.0 * MB;
/// The share of a table's data in large-value pages that makes them, not the rows, its size.
const LARGE_VALUE_SHARE: f64 = 0.5;
/// Pages a statement reads a run, and for each row it returns or changes, that mean it pulls whole rows: 16 pages is
/// 128 KB a row, far more than any index or record needs.
const WHOLE_ROW_PAGES: f64 = 50.0;
const WHOLE_ROW_PAGES_PER_ROW: f64 = 16.0;
/// Whole-row reads this often a day cost more than the table's size alone.
const WHOLE_ROW_RUNS_PER_DAY: f64 = 1_000.0;
/// Growth that matters: a tenth of the table a day, and at least this much.
const FAST_GROWTH_SHARE: f64 = 0.1;
const FAST_GROWTH_BYTES: f64 = 100.0 * MB;
/// A table this big is costly to keep, back up and restore whatever reads it.
const BIG_TABLE_BYTES: f64 = 10.0 * GB;
/// A data page, on SQL Server and PostgreSQL.
const PAGE_BYTES: f64 = 8_192.0;

/// A table's own data and the part of it in large-value pages: its in-row, large-value and row-overflow pages when the
/// engine splits them. `None` when it doesn't.
pub(super) fn split_bytes(t: &TableStat) -> Option<(f64, f64)> {
    let large = t.lob_bytes? + t.overflow_bytes.unwrap_or(0.0);
    let total = match t.in_row_bytes {
        Some(in_row) => in_row + large,
        None => t.data_bytes?,
    };
    (total > 0.0).then_some((total, large))
}

/// A column type whose values the engine keeps apart from the row once they're large.
pub(super) fn large_value_type(engine: Engine, data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    match engine {
        Engine::Mssql => {
            t.ends_with("(max)") || matches!(t.as_str(), "xml" | "text" | "ntext" | "image" | "geography" | "geometry")
        }
        Engine::Postgres | Engine::Yugabyte => {
            matches!(t.as_str(), "text" | "bytea" | "json" | "jsonb" | "xml" | "varchar" | "character varying")
        }
        Engine::Mysql | Engine::Sqlite => false,
    }
}

/// Statements that read or rewrite whole rows of `t`: many pages a run, and many for each row they return or change.
/// One-off statements (a single run) aren't the application's. A scan of the rows' own pages reads about as many
/// pages as the table keeps in-row, and returns few rows (a count, a filter on an unindexed column): many pages a row
/// without a single large value. A statement reading about that many pages a run is left out; one reading far fewer
/// (a few rows by key) or more (the large values too) isn't.
pub(super) fn whole_row_statements<'a>(
    snapshot: &'a Snapshot,
    t: &TableStat,
    size: f64,
    large: f64,
) -> Vec<&'a QueryStat> {
    let in_row_pages = t.in_row_bytes.unwrap_or(size - large).max(0.0) / PAGE_BYTES;
    let scans_in_row = |pages: f64| pages >= in_row_pages * 0.5 && pages <= in_row_pages * 1.2;
    let mut found: Vec<&QueryStat> = queries_touching(snapshot, &t.schema, &t.name)
        .into_iter()
        .filter(|q| {
            if is_monitoring_query(q) || q.executions < 2.0 {
                return false;
            }
            let Some(reads) = q.logical_reads else { return false };
            let per_run = reads / q.executions;
            let rows = q.rows.map(|r| r / q.executions).unwrap_or(1.0).max(1.0);
            per_run >= WHOLE_ROW_PAGES && per_run / rows >= WHOLE_ROW_PAGES_PER_ROW && !scans_in_row(per_run)
        })
        .collect();
    found.sort_by(|a, b| b.logical_reads.unwrap_or(0.0).total_cmp(&a.logical_reads.unwrap_or(0.0)));
    found.truncate(5);
    found
}

/// Whether a statement writes rows rather than reads them: its first statement keyword, past any parameter
/// declarations and comments the engine kept with the text.
pub(super) fn writes_rows(text: &str) -> bool {
    first_verb(text).is_some_and(|(_, k)| k != "select")
}

/// A table's average row and the share of its data in large-value pages, when its rows are mostly large values (the
/// bar large-rows holds them to, whatever the table's size): an insert or an update that sets them logs them in full.
pub(super) fn large_values(t: &TableStat) -> Option<(f64, f64)> {
    let rows = table_rows(t)?;
    let (size, large) = split_bytes(t)?;
    let versions = rows + t.dead_rows.unwrap_or(0.0).max(0.0);
    (size / versions >= LARGE_ROW_BYTES && large / size >= LARGE_VALUE_SHARE).then(|| (size / rows, large / size))
}

/// The rows a table holds, as far as its size per row goes. PostgreSQL's live-row counter starts again from zero after
/// a statistics reset, a crash or a failover, while the planner's count (reltuples) doesn't: the larger of the two.
pub(super) fn table_rows(t: &TableStat) -> Option<f64> {
    let planner = t.storage.as_ref().and_then(|s| s.planner_rows);
    match (t.rows, planner) {
        (Some(live), Some(planner)) => Some(live.max(planner)),
        (rows, planner) => rows.or(planner),
    }
    .filter(|r| *r >= 1.0)
}

/// The statements a large-rows finding lists, as their sentence says them: how many, whether they read or write, how
/// often, and the pages a run when every one counted its reads.
pub struct StatementsBelow {
    count: usize,
    /// As the sentence's `select` reads it: reads, read, writes, write, or both.
    verb: &'static str,
    how_often: String,
    pages: Option<f64>,
}

/// [`StatementsBelow`] for `statements`, `None` without one. Refine says it again once a writer joins them.
fn statements_below(snapshot: &Snapshot, statements: &[&QueryStat]) -> Option<StatementsBelow> {
    if statements.is_empty() {
        return None;
    }
    let runs: f64 = statements.iter().map(|q| q.executions).sum();
    let reads: Vec<f64> = statements.iter().filter_map(|q| q.logical_reads).collect();
    let writes = statements.iter().filter(|q| writes_rows(&q.text)).count();
    let one = statements.len() == 1;
    let verb = match (writes, one) {
        (0, true) => "reads",
        (0, false) => "read",
        (1, true) => "writes",
        (w, false) if w == statements.len() => "write",
        _ => "both",
    };
    Some(StatementsBelow {
        count: statements.len(),
        verb,
        how_often: runs_of_all(snapshot, statements),
        pages: (reads.len() == statements.len()).then(|| (reads.iter().sum::<f64>() / runs.max(1.0)).round()),
    })
}

/// A table of large rows, or a large table's columns of legacy json.
pub enum LargeRows<'s> {
    Table(LargeTable<'s>),
    Json(JsonColumns<'s>),
}

/// A table whose rows average many kilobytes, most of it in large values.
pub struct LargeTable<'s> {
    table: &'s TableStat,
    rows: f64,
    size: f64,
    large: f64,
    /// The columns whose type can hold large values, in their order.
    columns: Vec<String>,
    /// The share of its row versions that are dead, when a fifth or more.
    dead: Option<f64>,
    statements: Vec<&'s QueryStat>,
    below: Option<StatementsBelow>,
    /// Whole-row statements a day, where every one's runs came a day at a time.
    runs_a_day: Option<f64>,
    growth: Option<Growth>,
    severity: Severity,
}

/// How the table's size changed since the newest scan at least an hour back: its time and the change.
pub struct Growth {
    taken_at: String,
    delta: f64,
    days: f64,
}

/// A large PostgreSQL table with columns of the legacy text-backed `json` type.
pub struct JsonColumns<'s> {
    table: &'s TableStat,
    rows: f64,
    columns: Vec<&'s str>,
}

fn large_rows(snapshot: &Snapshot) -> Vec<LargeRows<'_>> {
    let mut cases: Vec<(LargeRows<'_>, Severity)> = snapshot
        .tables
        .iter()
        .filter_map(|t| {
            let rows = table_rows(t)?;
            let (size, large) = split_bytes(t)?;
            // Dead row versions (not yet vacuumed) keep their large values on disk too: a queue whose deleted payloads
            // wait for VACUUM isn't a table of large rows. Per version, they must still be large.
            let versions = rows + t.dead_rows.unwrap_or(0.0).max(0.0);
            (size >= LARGE_ROWS_TABLE_BYTES && size / versions >= LARGE_ROW_BYTES && large / size >= LARGE_VALUE_SHARE)
                .then(|| large_table(snapshot, t, rows, size, large, None))
        })
        .map(|table| {
            let severity = table.severity;
            (LargeRows::Table(table), severity)
        })
        .collect();
    cases.extend(json_columns(snapshot).into_iter().map(|json| (LargeRows::Json(json), Severity::Low)));
    // Ranked by severity alone, keeping their order within one.
    cases.sort_by(|a, b| base_score(b.1).total_cmp(&base_score(a.1)));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

/// Large PostgreSQL tables (`>= 100k` rows) that use the legacy text-backed `json` type instead of binary `jsonb`.
fn json_columns(snapshot: &Snapshot) -> Vec<JsonColumns<'_>> {
    if snapshot.server.engine != Engine::Postgres || snapshot.columns.is_empty() {
        return Vec::new();
    }
    snapshot
        .tables
        .iter()
        .filter(|t| t.partition_of.is_none())
        .filter_map(|t| {
            let rows = table_rows(t).filter(|r| *r >= 100_000.0)?;
            let columns: Vec<&str> = snapshot
                .columns
                .iter()
                .filter(|c| {
                    c.schema.eq_ignore_ascii_case(&t.schema)
                        && c.table.eq_ignore_ascii_case(&t.name)
                        && c.data_type.trim().eq_ignore_ascii_case("json")
                })
                .map(|c| c.name.as_str())
                .collect();
            (!columns.is_empty()).then_some(JsonColumns { table: t, rows, columns })
        })
        .collect()
}

/// What a table of large rows establishes, with the statements refine listed (`listed`) or those that read whole rows.
fn large_table<'s>(
    snapshot: &'s Snapshot,
    t: &'s TableStat,
    rows: f64,
    size: f64,
    large: f64,
    listed: Option<&[&'s QueryStat]>,
) -> LargeTable<'s> {
    let engine = snapshot.server.engine;
    // The columns that can hold them, from the catalog: nothing is sampled. Which of them hold the bytes isn't known
    // without reading the data: these are the columns that can.
    let mut columns: Vec<_> = snapshot
        .columns
        .iter()
        .filter(|c| {
            c.schema.eq_ignore_ascii_case(&t.schema)
                && c.table.eq_ignore_ascii_case(&t.name)
                && large_value_type(engine, &c.data_type)
        })
        .collect();
    columns.sort_by_key(|c| c.ordinal);
    // Statements reading whole rows: what makes the size cost on every run, not just on disk.
    let statements =
        listed.map(|queries| queries.to_vec()).unwrap_or_else(|| whole_row_statements(snapshot, t, size, large));
    let runs: f64 = statements.iter().map(|q| q.executions).sum();
    let hot = runs_at_least(snapshot, &statements, WHOLE_ROW_RUNS_PER_DAY);
    // A day's worth only where every statement's runs came that way.
    let runs_a_day = snapshot
        .workload
        .as_ref()
        .filter(|w| statements.iter().all(|q| w.runs_per_day_of(q).is_some()))
        .and_then(|w| w.per_day_measured(runs))
        .filter(|_| !statements.is_empty());
    // Growth since the newest scan at least an hour back, when there is one: named by its time, since it is not
    // always the scan just before.
    let name = t.qualified();
    let growth = snapshot.sizes_before.as_ref().and_then(|b| {
        let then = b.bytes(&name)?;
        let days = days_between(&b.taken_at, &snapshot.taken_at).filter(|d| *d >= 1.0 / 24.0)?;
        Some(Growth { taken_at: b.taken_at.clone(), delta: size - then, days })
    });
    let fast = growth.as_ref().is_some_and(|g| {
        let a_day = g.delta / g.days;
        a_day >= FAST_GROWTH_BYTES && a_day >= FAST_GROWTH_SHARE * size
    });
    let weight = usize::from(hot) + usize::from(fast) + usize::from(size >= BIG_TABLE_BYTES);
    LargeTable {
        table: t,
        rows,
        size,
        large,
        columns: columns.iter().map(|c| c.name.clone()).collect(),
        // Dead versions hold their values until VACUUM: said how much of the table they are.
        dead: dead_share(t).filter(|d| *d >= 0.2),
        below: statements_below(snapshot, &statements),
        statements,
        runs_a_day,
        growth,
        severity: match weight {
            0 => Severity::Low,
            1 => Severity::Medium,
            _ => Severity::High,
        },
    }
}

/// Render the original table evidence with the complete list retained after refinement. No sentence matching.
pub(crate) fn restate_large_rows(snapshot: &Snapshot, f: &Finding, listed: &[&QueryStat]) -> Option<String> {
    let facts = f.facts.large_rows.as_ref()?;
    let table = snapshot.tables.iter().find(|t| t.qualified().eq_ignore_ascii_case(&f.subject.name))?;
    let case = large_table(snapshot, table, facts.rows, facts.size, facts.large, Some(listed));
    Some(large_table_finding(snapshot, case).summary)
}

fn large_rows_finding(snapshot: &Snapshot, case: LargeRows<'_>) -> Finding {
    match case {
        LargeRows::Table(table) => large_table_finding(snapshot, table),
        LargeRows::Json(json) => json_finding(json),
    }
}

fn json_finding(json: JsonColumns<'_>) -> Finding {
    let JsonColumns { table: t, rows, columns } = json;
    let qual = t.qualified();
    let first_col = columns[0];
    let mut f = finding(
        "large-rows",
        &format!("json-column:{qual}.{}", columns.join(",")),
        Category::Maintenance,
        subject(SubjectKind::Table, &qual),
    );
    f.actionability = culprit_core::Actionability::OptionalImprovement;
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.set_label(t!("check.large_rows.json.impact"));
    f.confidence = Confidence::High;
    f.title =
        t!("check.large_rows.json.title", table = &t.name, columns = columns.join(", "), rows = format::count(rows));
    f.summary =
        t!("check.large_rows.json.summary", table = &qual, rows = format::count(rows), columns = columns.join("`, `"));
    f.metrics.push(Metric::said(culprit_core::key!("check.large_rows.json.metric.rows"), rows, Unit::Rows));
    f.metrics.push(Metric::said_text(
        culprit_core::key!("check.large_rows.json.metric.column"),
        format!("{}.{first_col} (json)", t.name),
    ));
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    let col_ident = quote(Engine::Postgres, first_col);
    f.fix = Some(Fix {
        summary: t!("check.large_rows.json.fix", columns = columns.join("`, `"), table = &qual),
        sql: Some(format!("ALTER TABLE {target} ALTER COLUMN {col_ident} TYPE jsonb USING {col_ident}::jsonb;")),
        cost: Some(t!("check.large_rows.json.fix_cost")),
    });
    f.verify = vec![t!("check.large_rows.json.verify", table = &qual, column = first_col)];
    f
}

fn large_table_finding(snapshot: &Snapshot, large: LargeTable<'_>) -> Finding {
    let LargeTable { table: t, rows, size, large: large_bytes, .. } = large;
    let engine = snapshot.server.engine;
    let name = t.qualified();
    let average = size / rows;
    let share = large_bytes / size;
    let mut f = finding("large-rows", &name, Category::Maintenance, subject(SubjectKind::Table, &name));
    f.facts.large_rows = Some(culprit_core::FindingLargeRows { rows, size, large: large_bytes });
    let (rows_said, size_said, average_said) = (format::count(rows), format::bytes(size), format::bytes(average));
    f.title =
        t!("check.large_rows.title", table = &t.name, size = &size_said, rows = &rows_said, average = &average_said);
    let shown: Vec<String> = large.columns.iter().take(6).cloned().collect();
    let mut summary = vec![match (shown.as_slice(), large.columns.len()) {
        ([], _) => t!(
            "check.large_rows.summary.size",
            rows = &rows_said,
            table = &t.name,
            size = &size_said,
            average = &average_said,
            share = share_words(share)
        ),
        ([one], 1) => t!(
            "check.large_rows.summary.size_column",
            rows = &rows_said,
            table = &t.name,
            size = &size_said,
            average = &average_said,
            share = share_words(share),
            column = one
        ),
        (names, n) if n > names.len() => t!(
            "check.large_rows.summary.size_columns_more",
            rows = &rows_said,
            table = &t.name,
            size = &size_said,
            average = &average_said,
            share = share_words(share),
            columns = names.join(", "),
            more = (n - names.len()).to_string()
        ),
        (names, _) => t!(
            "check.large_rows.summary.size_columns",
            rows = &rows_said,
            table = &t.name,
            size = &size_said,
            average = &average_said,
            share = share_words(share),
            columns = format::list(names)
        ),
    }];
    if engine == Engine::Postgres {
        summary.push(t!("check.large_rows.summary.compressed"));
    }
    if let Some(dead) = large.dead {
        summary.push(t!("check.large_rows.summary.dead", share = format::percent(dead)));
    }
    if let Some(below) = &large.below {
        summary.push(match below.pages {
            Some(p) => t!(
                "check.large_rows.summary.statements_pages",
                n = below.count,
                verb = below.verb,
                how_often = &below.how_often,
                pages = format::count(p),
                bytes = format::bytes(p * 8192.0)
            ),
            None => t!(
                "check.large_rows.summary.statements",
                n = below.count,
                verb = below.verb,
                how_often = &below.how_often
            ),
        });
    }
    if let Some(g) = &large.growth {
        let when = crate::queries::when_phrase(&g.taken_at);
        let a_day = g.delta / g.days;
        summary.push(if g.delta >= MB && g.days >= 20.0 / 24.0 {
            f.metrics.push(Metric::said(culprit_core::key!("check.large_rows.metric.growth"), a_day, Unit::Bytes));
            t!(
                "check.large_rows.summary.grew_a_day",
                delta = format::bytes(g.delta),
                when = &when,
                a_day = format::bytes(a_day)
            )
        } else if g.delta >= MB {
            t!(
                "check.large_rows.summary.grew",
                delta = format::bytes(g.delta),
                span = culprit_core::span_words(g.days * 24.0),
                when = &when
            )
        } else if g.delta <= -MB {
            t!("check.large_rows.summary.smaller", delta = format::bytes(-g.delta), when = &when)
        } else {
            t!("check.large_rows.summary.not_grown", when = &when)
        });
    }
    f.summary = summary.join(" ");
    f.severity = large.severity;
    f.confidence = Confidence::High;
    let statements = large.statements;
    if let Some(impact) = query_impact(snapshot, &statements) {
        f.impact.db_time_share = Some(impact.share);
        f.impact.time_ms_per_day = impact.per_day_ms;
    }
    severity_score(&mut f);
    f.impact.set_label(t!("check.large_rows.impact", size = &size_said, average = &average_said));
    let mut metrics = vec![
        Metric::said(culprit_core::key!("check.large_rows.metric.table_data"), size, Unit::Bytes),
        Metric::said(culprit_core::key!("common.metric.rows"), rows, Unit::Rows),
        Metric::said(culprit_core::key!("check.large_rows.metric.average_row"), average, Unit::Bytes),
        Metric::said(culprit_core::key!("check.large_rows.metric.large_pages"), share, Unit::Ratio),
    ];
    if let Some(runs_a_day) = large.runs_a_day {
        metrics.push(Metric::said(
            culprit_core::key!("check.large_rows.metric.whole_row_a_day"),
            runs_a_day,
            Unit::Count,
        ));
    }
    metrics.append(&mut f.metrics);
    f.metrics = metrics;
    f.queries = query_refs(&statements);
    f.last_seen = statements.iter().filter_map(|q| q.last_seen.clone()).max();
    f.fix = Some(Fix {
        summary: [
            t!("check.large_rows.fix.read_columns"),
            t!("check.large_rows.fix.compress", engine = culprit_core::wire_name(&engine)),
        ]
        .join(" "),
        sql: None,
        cost: None,
    });
    let compare_time = if statements.is_empty() {
        t!("check.large_rows.verify.record_runs")
    } else {
        crate::queries::what_changed_between_scans(
            snapshot,
            &t!("check.large_rows.verify.changed_statements"),
            &t!("check.large_rows.verify.compare_time_per_run"),
        )
        .unwrap_or_else(|| t!("check.large_rows.verify.compare_windows"))
    };
    f.verify = vec![
        [t!("check.large_rows.verify.read_fewer"), compare_time, t!("check.large_rows.verify.rewritten")].join(" "),
        t!("check.large_rows.verify.smaller_values"),
    ];
    f.caveats = vec![t!("check.large_rows.caveat.reserved")];
    f
}
