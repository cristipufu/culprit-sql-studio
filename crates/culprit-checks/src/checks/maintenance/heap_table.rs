//! The `heap-table` check: Heaps grow forwarded records and can't be read in key order; most OLTP tables should be clustered.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn heap_table_info() -> CheckInfo {
    CheckInfo {
        id: "heap-table".into(),
        title: t!("check.heap_table.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Mssql],
        requires: vec![Capability::TableStats],
        description: t!("check.heap_table.info.description"),
    }
}

check!(
    HeapTableCheck: heap_table_info,
    decide = heap_tables,
    render = heap_table_finding,
    case<'s> = HeapTable<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::Heap)],
);

/// A large SQL Server table stored as a heap, and what its forwarded reads establish.
pub struct HeapTable<'s> {
    table: &'s culprit_core::TableStat,
    rows: f64,
    /// Reads that followed a forwarding pointer to another page, since the counters started.
    forwarded: Option<f64>,
    /// Those reads a day, said as such only over 20 hours of counters; under that, what was counted and since when.
    said_per_day: Option<f64>,
    /// The title is about the forwarded reads (a thousand a day or more): (their count, over these days of counters).
    title_forwarded: Option<(f64, f64)>,
    severity: Severity,
}

/// The heaps worth a finding: 100K rows or more.
fn heap_tables(snapshot: &Snapshot) -> Vec<HeapTable<'_>> {
    if snapshot.server.engine != Engine::Mssql {
        return Vec::new();
    }
    snapshot
        .tables
        .iter()
        .filter(|t| t.is_heap == Some(true) && t.rows.unwrap_or(0.0) >= 100_000.0)
        .map(|table| {
            let forwarded = table.forwarded_fetches.filter(|f| *f > 0.0);
            let days = counter_days_for_rate(snapshot);
            let forwarded_per_day = forwarded.and_then(|f| counter_rate_per_day(snapshot, f));
            let title_forwarded = match (forwarded_per_day, forwarded, days) {
                (Some(per_day), Some(total), Some(days)) if per_day >= 1000.0 => Some((total, days)),
                _ => None,
            };
            HeapTable {
                table,
                rows: table.rows.unwrap_or(0.0),
                forwarded,
                said_per_day: forwarded_per_day.filter(|_| days.is_some_and(per_day_is_measured)),
                title_forwarded,
                // What a heap costs here is the extra reads it causes, not its size: an insert-only log heap costs
                // nothing.
                severity: if forwarded_per_day.is_some_and(|d| d >= 100_000.0) {
                    Severity::Medium
                } else {
                    Severity::Low
                },
            }
        })
        .collect()
}

fn heap_table_finding(snapshot: &Snapshot, heap: HeapTable<'_>) -> Finding {
    let t = heap.table;
    let table = t.qualified();
    let mut f = finding("heap-table", &table, Category::Maintenance, subject(SubjectKind::Table, &table));
    f.title = match heap.title_forwarded {
        Some((total, days)) => t!(
            "check.heap_table.title.forwarded",
            table = &t.name,
            how_many = how_many(snapshot, total, days, culprit_core::key!("common.noun.time"))
        ),
        None => t!("check.heap_table.title.no_clustered_index", table = &t.name, rows = format::count(heap.rows)),
    };
    let mut summary = vec![t!("check.heap_table.summary.heap", table = &table)];
    if let Some(total) = heap.forwarded {
        let count = format::count(total);
        summary.push(match heap.said_per_day {
            Some(per_day) => t!(
                "check.heap_table.summary.forwarded_per_day",
                count = count,
                since = since_phrase(snapshot),
                per_day = format::count(per_day)
            ),
            None => t!("check.heap_table.summary.forwarded", count = count, since = since_phrase(snapshot)),
        });
    }
    summary.push(if t.has_primary_key {
        t!("check.heap_table.summary.primary_key")
    } else {
        t!("check.heap_table.summary.no_primary_key")
    });
    f.summary = summary.join(" ");
    f.severity = heap.severity;
    if let Some(d) = heap.said_per_day {
        f.metrics.push(Metric::said(culprit_core::key!("check.heap_table.metric.forwarded_per_day"), d, Unit::Count));
    }
    f.impact.score = base_score(f.severity);
    f.impact.set_label(t!("check.heap_table.impact", rows = format::count(heap.rows)));
    f.confidence = Confidence::Medium;
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.rows"), heap.rows, Unit::Rows));
    if let Some(b) = t.total_bytes {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.size"), b, Unit::Bytes));
    }
    f.fix = Some(Fix {
        summary: t!("check.heap_table.fix"),
        sql: None,
        cost: t.total_bytes.map(|b| t!("check.heap_table.fix_cost", size = format::bytes(b))),
    });
    f.caveats.push(t!("check.heap_table.caveat.staging"));
    f
}
