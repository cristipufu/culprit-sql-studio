//! The `big-result` check: Reads that send tens of thousands of rows to the application on every run, with an estimate in bytes from the plan's row width where the engine gives one.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn big_result_info() -> CheckInfo {
    CheckInfo {
        id: "big-result".into(),
        title: t!("check.big_result.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![],
        description: t!("check.big_result.info.description"),
    }
}

check!(
    BigResultCheck: big_result_info,
    decide = big_results,
    render = big_result_finding,
    case<'s> = BigResult<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Rows)],
);

/// What a statement's big result adds to the spill finding on it, which carries both (refine's "a spill carries the
/// result it sorts"): one statement, one share, one finding. Its summary says the rows each run sends, its fix how to
/// return fewer, and its verify what a later scan shows once it does (nothing for an export, whose rows are the point).
/// The summary and fix start with a space: they follow the spill's own sentences.
pub(crate) struct ResultOfASpill {
    pub summary: String,
    pub fix: String,
    pub verify: Option<String>,
}

/// The rows a run of `q` sends, and their bytes where the plan's row width is known: "80K rows (≈ 11.4 MB)".
fn sends(rows: f64, width: Option<f64>) -> String {
    match width {
        Some(w) => {
            t!("check.big_result.sends_bytes", n = rows, rows = format::count(rows), bytes = format::bytes(w * rows))
        }
        None => t!("check.big_result.sends", n = rows, rows = format::count(rows)),
    }
}

/// The width of one of `q`'s rows: what the engine measured, else its plan's estimate.
fn row_width(q: &QueryStat) -> Option<f64> {
    q.costs.as_ref().and_then(|c| c.row_bytes).or_else(|| q.plan.as_ref().and_then(plan_row_bytes))
}

/// [`ResultOfASpill`] for `q`, worded the way [`big_result_finding`] words it; `None` when its runs return no rows count.
pub(crate) fn result_of_a_spill(snapshot: &Snapshot, q: &QueryStat) -> Option<ResultOfASpill> {
    let rows = per_run(q.rows?, q.executions);
    let sends = sends(rows, row_width(q));
    Some(if looks_like_export(snapshot, q) {
        ResultOfASpill {
            summary: format!(" {}", t!("check.big_result.spill.summary_export", sends = sends)),
            fix: format!(" {}", t!("check.big_result.spill.fix_export")),
            verify: None,
        }
    } else {
        ResultOfASpill {
            summary: format!(" {}", t!("check.big_result.spill.summary", sends = sends)),
            fix: format!(" {}", t!("check.big_result.spill.fix")),
            // On running totals the spill's own step names the two scans to compare and says why, once.
            verify: Some(if snapshot.workload.as_ref().is_some_and(|w| !w.is_windowed()) {
                t!("check.big_result.spill.verify_between_scans")
            } else {
                t!("check.big_result.spill.verify_rescan")
            }),
        }
    })
}

/// A read that sends 10,000 rows or more to the application on every run.
pub struct BigResult<'s> {
    q: &'s QueryStat,
    /// Rows a run.
    rows: f64,
    /// A row's bytes, measured or from the plan.
    width: Option<f64>,
    /// It copies a whole table out: an export, a dump or an ETL job, whose rows are the point.
    export: bool,
    severity: Severity,
}

fn big_results(snapshot: &Snapshot) -> Vec<BigResult<'_>> {
    let mut cases: Vec<(BigResult<'_>, f64)> = app_queries(snapshot)
        .filter(|q| returns_rows(q) && q.executions >= 20.0)
        .filter_map(|q| {
            let rows = per_run(q.rows?, q.executions);
            (rows >= 10_000.0).then_some((q, rows))
        })
        .map(|(q, rows)| {
            let export = looks_like_export(snapshot, q);
            let severity = if export {
                Severity::Low
            } else if rows >= 100_000.0 || window_at_least(snapshot, q, q.rows.unwrap_or(0.0), 100_000_000.0) {
                Severity::Medium
            } else {
                Severity::Low
            };
            let case = BigResult { q, rows, width: row_width(q), export, severity };
            (case, query_score(snapshot, q, severity))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn big_result_finding(snapshot: &Snapshot, big: BigResult<'_>) -> Finding {
    let BigResult { q, rows, width, export, .. } = big;
    let mut f = finding("big-result", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = capitalize(&query_handle(q));
    let bytes = width.map(|w| w * rows);
    f.title = t!("check.big_result.title", handle = &handle, tense = tense(snapshot, &[q]), sends = sends(rows, width));
    let (each, runs, ran, all) = (
        format::count(rows),
        format::count(q.executions),
        ran_phrase(snapshot, q),
        format::count(q.rows.unwrap_or(0.0)),
    );
    f.summary = [
        match (bytes, width) {
            (Some(b), Some(w)) => t!(
                "check.big_result.summary.sends_bytes",
                n = rows,
                rows = each,
                bytes = format::bytes(b),
                width = format::bytes(w),
                runs_n = q.executions,
                runs = runs,
                ran = ran,
                total_n = q.rows.unwrap_or(0.0),
                total = all
            ),
            _ => t!(
                "check.big_result.summary.sends",
                n = rows,
                rows = each,
                runs_n = q.executions,
                runs = runs,
                ran = ran,
                total_n = q.rows.unwrap_or(0.0),
                total = all
            ),
        },
        if export {
            t!("check.big_result.summary.export", how_often = runs_phrase(snapshot, q.executions))
        } else {
            t!("check.big_result.summary.too_many")
        },
    ]
    .join(" ");
    f.severity = big.severity;
    with_query(snapshot, &mut f, q);
    severity_score(&mut f);
    f.impact.label = t!("check.big_result.impact", n = rows, rows = format::count(rows));
    f.confidence = if width.is_some() { Confidence::High } else { Confidence::Medium };
    f.metrics = vec![
        Metric::new(t!("check.big_result.metric.rows_per_run"), rows, Unit::Rows),
        Metric::said(key!("common.metric.runs"), q.executions, Unit::Count),
    ];
    if let Some(b) = bytes {
        f.metrics.push(Metric::new(t!("check.big_result.metric.bytes_per_run"), b, Unit::Bytes));
    }
    f.fix = Some(Fix {
        summary: if export { t!("check.big_result.fix.export") } else { t!("check.big_result.fix.return_used") },
        sql: None,
        cost: None,
    });
    // On a source of running totals, what two scans show of the rows it returns between them; else a later scan's
    // rows per run.
    let now = t!(
        "check.big_result.verify.now",
        n = q.rows.unwrap_or(0.0),
        rows = format::count(q.rows.unwrap_or(0.0)),
        runs_n = q.executions,
        runs = format::count(q.executions)
    );
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.big_result.verify.between_scans", source = source, now = now),
        None => t!("check.big_result.verify.rescan"),
    }];
    f
}
