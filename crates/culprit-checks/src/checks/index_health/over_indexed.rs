//! The `over-indexed` check: Tables with many secondary indexes that are written a lot: every insert updates each index.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn over_indexed_info() -> CheckInfo {
    CheckInfo {
        id: "over-indexed".into(),
        title: t!("check.over_indexed.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![Capability::TableStats],
        description: t!("check.over_indexed.info.description"),
    }
}

check!(
    OverIndexedCheck: over_indexed_info,
    decide = over_indexed_tables,
    render = over_indexed_finding,
    case<'s> = OverIndexed<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Tables(crate::evidence::TableMeasure::Writes)],
);

pub(crate) const MANY_INDEXES: usize = 7;

/// Writes a day that make many secondary indexes a cost worth a finding: statements on SQL Server, rows elsewhere
/// (see [`IndexWrites`]). A statement writes at least a row, so the bar is never looser on SQL Server.
pub(crate) const MANY_WRITES_A_DAY: f64 = 50_000.0;

/// The writes to a table that maintain its secondary indexes, since its counters started, and what the count counts.
pub(crate) struct IndexWrites {
    pub(crate) total: f64,
    pub(crate) days: f64,
    /// SQL Server counts write statements on the clustered index (`user_updates`), whatever rows each touched;
    /// PostgreSQL and MySQL count rows.
    pub(crate) statements: bool,
}

impl IndexWrites {
    /// The counted noun's key ([`crate::support::counted`]): write statements on SQL Server, rows written elsewhere.
    pub(crate) fn unit(&self) -> &'static str {
        if self.statements {
            culprit_core::key!("common.noun.write_statement")
        } else {
            culprit_core::key!("common.noun.row_written")
        }
    }
}

/// [`measured_writes`] without the updates that maintain no index: PostgreSQL's HOT updates change no indexed column
/// and stay on their page, so they write no index entry at all.
pub(crate) fn index_writes(snapshot: &Snapshot, t: &culprit_core::TableStat) -> Option<IndexWrites> {
    let (total, days) = measured_writes(snapshot, &t.schema, &t.name)?;
    let hot = match snapshot.server.engine {
        Engine::Postgres => t.storage.as_ref().and_then(|s| s.hot_updates).unwrap_or(0.0).min(t.updates.unwrap_or(0.0)),
        Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => 0.0,
    };
    Some(IndexWrites { total: (total - hot).max(0.0), days, statements: snapshot.server.engine == Engine::Mssql })
}

/// A write-heavy table with many secondary indexes, and what its finding concludes.
pub struct OverIndexed<'s> {
    table: &'s culprit_core::TableStat,
    /// Its secondary indexes that writes maintain.
    secondary: usize,
    writes: IndexWrites,
    /// The writes are said a day (over 20 hours of counters), otherwise as counted.
    measured: bool,
    /// What each write does to the indexes, as the engine keeps them.
    upkeep: Upkeep,
    /// Those nothing reads, a wider one covers, or that are hidden or invalid and still maintained: where to start.
    idle: Vec<&'s IndexStat>,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

/// What a table's writes do to its secondary indexes.
enum Upkeep {
    /// SQL Server counts write statements, each maintaining every index for each row it changes.
    Statements,
    /// PostgreSQL adds an index entry on every insert and every update that isn't HOT.
    Entries,
    /// Elsewhere, every insert and every update touching an index's columns changes it.
    Changes,
}

fn over_indexed_tables(snapshot: &Snapshot) -> Vec<OverIndexed<'_>> {
    // By table and name: MySQL and SQL Server index names are unique only within a table.
    let redundant: Vec<(&str, &str, &str)> = redundant_indexes(snapshot)
        .iter()
        .map(|(a, _)| (a.schema.as_str(), a.table.as_str(), a.name.as_str()))
        .collect();
    let mut out = Vec::new();
    // A partition carries its parent's indexes: one finding per partition would be one cause many times.
    for t in snapshot.tables.iter().filter(|t| t.partition_of.is_none()) {
        let secondary: Vec<&IndexStat> = snapshot
            .indexes_on(&t.schema, &t.name)
            .filter(|i| !i.is_primary_key && !i.is_clustered && i.maintained.unwrap_or(!i.is_disabled))
            .collect();
        if secondary.len() < MANY_INDEXES {
            continue;
        }
        let Some(w) = index_writes(snapshot, t) else { continue };
        // Judged per day; said per day only over 20 hours of counters.
        let writes = w.total / w.days;
        if writes < MANY_WRITES_A_DAY {
            continue;
        }
        let measured = counters_measure_a_day(snapshot, w.days);
        let idle: Vec<&IndexStat> = secondary
            .iter()
            .copied()
            .filter(|i| {
                is_unused(snapshot, i)
                    || redundant.contains(&(i.schema.as_str(), i.table.as_str(), i.name.as_str()))
                    || (i.is_disabled
                        && i.maintained == Some(true)
                        && !i.is_unique
                        && !too_new(snapshot, i)
                        && (snapshot.server.engine == Engine::Postgres || i.reads == Some(0.0)))
            })
            .collect();
        let severity = if idle.is_empty() { Severity::Low } else { Severity::Medium };
        out.push(OverIndexed {
            table: t,
            secondary: secondary.len(),
            writes: w,
            measured,
            upkeep: match snapshot.server.engine {
                Engine::Mssql => Upkeep::Statements,
                Engine::Postgres => Upkeep::Entries,
                Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => Upkeep::Changes,
            },
            severity,
            score: base_score(severity) + (writes.log10() - 4.0).max(0.0) * 3.0,
            confidence: if idle.is_empty() { Confidence::Low } else { Confidence::Medium },
            idle,
        });
    }
    out
}

fn over_indexed_finding(snapshot: &Snapshot, o: OverIndexed<'_>) -> Finding {
    let t = o.table;
    let w = &o.writes;
    let (total, days) = (w.total, w.days);
    let writes = total / days;
    let count = o.secondary.to_string();
    let table_name = t.qualified();
    let mut f = finding("over-indexed", &table_name, Category::Indexes, subject(SubjectKind::Table, &table_name));
    f.title = t!(
        "check.over_indexed.title",
        table = &t.name,
        count = &count,
        how_many = how_many(snapshot, total, days, w.unit())
    );
    let index_writes = |noun: &str| how_many(snapshot, total * o.secondary as f64, days, noun);
    let upkeep = match o.upkeep {
        Upkeep::Statements => t!(
            "check.over_indexed.summary.upkeep_statements",
            table = &table_name,
            count = &count,
            how_many = how_many(snapshot, total, days, w.unit())
        ),
        Upkeep::Entries => t!(
            "check.over_indexed.summary.upkeep_entries",
            table = &table_name,
            count = &count,
            how_many = index_writes(culprit_core::key!("common.noun.index_entry"))
        ),
        Upkeep::Changes => t!(
            "check.over_indexed.summary.upkeep_changes",
            table = &table_name,
            count = &count,
            how_many = index_writes(culprit_core::key!("common.noun.index_change"))
        ),
    };
    let start = match o.idle.as_slice() {
        [] => t!("check.over_indexed.summary.all_read"),
        [one] => t!("check.over_indexed.summary.one_idle", index = &one.name),
        many => t!(
            "check.over_indexed.summary.idle",
            count = many.len().to_string(),
            indexes = format::list(&many.iter().map(|i| i.name.clone()).collect::<Vec<_>>())
        ),
    };
    f.summary = [upkeep, start].join(" ");
    f.severity = o.severity;
    f.impact.score = o.score;
    f.impact.set_label(match (w.statements, o.measured) {
        (true, true) => {
            t!("check.over_indexed.impact.statements_per_day", count = &count, writes = format::count(writes))
        }
        (false, true) => t!("check.over_indexed.impact.rows_per_day", count = &count, writes = format::count(writes)),
        (true, false) => t!("check.over_indexed.impact.statements", count = &count, writes = format::count(total)),
        (false, false) => t!("check.over_indexed.impact.rows", count = &count, writes = format::count(total)),
    });
    f.confidence = o.confidence;
    f.metrics.push(Metric::said(
        culprit_core::key!("check.over_indexed.metric.secondary"),
        o.secondary as f64,
        Unit::Count,
    ));
    f.metrics.push(match (w.statements, o.measured) {
        (true, true) => {
            Metric::said(culprit_core::key!("check.over_indexed.metric.statements_per_day"), writes, Unit::Count)
        }
        (false, true) => {
            Metric::said(culprit_core::key!("check.over_indexed.metric.rows_per_day"), writes, Unit::Count)
        }
        (true, false) => Metric::said(culprit_core::key!("check.over_indexed.metric.statements"), total, Unit::Count),
        (false, false) => Metric::said(culprit_core::key!("check.over_indexed.metric.rows"), total, Unit::Count),
    });
    f.metrics.push(Metric::said(
        culprit_core::key!("check.over_indexed.metric.idle"),
        o.idle.len() as f64,
        Unit::Count,
    ));
    f.fix = Some(Fix {
        summary: if o.idle.is_empty() {
            t!("check.over_indexed.fix.review")
        } else if o.idle.iter().all(|i| i.is_disabled) {
            t!("check.over_indexed.fix.drop_hidden")
        } else {
            t!("check.over_indexed.fix.hide")
        },
        sql: (!o.idle.is_empty())
            .then(|| o.idle.iter().map(|i| hide_index_sql(snapshot, i)).collect::<Vec<_>>().join("\n")),
        cost: None,
    });
    f.verify = vec![t!("check.over_indexed.verify", table = &t.name)];
    f
}
