//! The `last-page-insert` check: Indexes keyed on an ever-increasing value (identity, sequence, insert time) where concurrent inserts wait for the latch on the last page (PAGELATCH_EX).

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::IndexContention;

pub fn last_page_insert_info() -> CheckInfo {
    CheckInfo {
        id: "last-page-insert".into(),
        title: t!("check.last_page_insert.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.last_page_insert.info.description"),
    }
}

check!(
    LastPageInsertCheck: last_page_insert_info,
    decide = last_page_latches,
    render = last_page_insert_finding,
    case<'s> = LastPageLatch<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Source(culprit_core::source_id::INDEX_CONTENTION)],
);

/// An index on an ever-growing key whose sessions waited a thousand times or more, and ten seconds or more, for page
/// latches.
pub struct LastPageLatch<'s> {
    index: &'s IndexStat,
    contention: &'s IndexContention,
    /// About as many inserts as waits (or inserts not counted): the waits are inserts on its last page. Otherwise
    /// they are sessions changing a few hot rows over and over.
    inserts: bool,
    severity: Severity,
    /// Without a rate a day, how long the counters have counted: too little for one, or unknown.
    history: Option<CounterAge>,
}

/// How long counters kept since the server started have counted, when that is too little to grade them by.
#[derive(Clone, Copy)]
pub enum CounterAge {
    /// Less than an hour.
    Short,
    /// The scan doesn't know when the server started.
    Unknown,
}

fn last_page_latches(snapshot: &Snapshot) -> Vec<LastPageLatch<'_>> {
    snapshot
        .indexes
        .iter()
        .filter_map(|i| {
            let c = i.contention.as_ref()?;
            (c.sequential_key
                && c.page_latch_waits >= 1_000.0
                && c.page_latch_wait_ms >= 10_000.0
                && c.optimize_for_sequential_key != Some(true))
            .then_some((i, c))
        })
        .map(|(index, c)| {
            // Inserts explain the waits only where there are about as many of them: a small counter or queue table
            // whose few rows are updated over and over waits on its hot pages without inserting much, and pacing
            // inserts wouldn't help it.
            let inserts = c.leaf_inserts.is_none_or(|n| n >= c.page_latch_waits);
            // Counted since the index was loaded, with no time for each wait: graded on a rate measured over an hour or
            // more (ten minutes a day High, one Medium), never on the total.
            let rate = counter_rate_per_day(snapshot, c.page_latch_wait_ms);
            let history = rate.is_none().then(|| match counter_days(snapshot) {
                Some(_) => CounterAge::Short,
                None => CounterAge::Unknown,
            });
            LastPageLatch {
                index,
                contention: c,
                inserts,
                severity: since_start_severity(rate, 600_000.0, 60_000.0),
                history,
            }
        })
        .collect()
}

fn last_page_insert_finding(snapshot: &Snapshot, latch: LastPageLatch<'_>) -> Finding {
    let (i, c) = (latch.index, latch.contention);
    let table = index_label(i);
    let mut f = finding(
        "last-page-insert",
        &format!("{table}.{}", i.name),
        Category::Indexes,
        subject(SubjectKind::Index, format!("{table}.{}", i.name)),
    );
    let (waits, time) = (format::count(c.page_latch_waits), format::duration(c.page_latch_wait_ms));
    if latch.inserts {
        f.title = t!("check.last_page_insert.title.inserts", table = &i.table, index = &i.name);
        f.summary = t!(
            "check.last_page_insert.summary.inserts",
            index = &i.name,
            column = i.columns.first().cloned().unwrap_or_default(),
            waits = waits,
            time = time
        );
    } else {
        f.title = t!("check.last_page_insert.title.hot_pages", index = &i.name);
        f.summary = t!(
            "check.last_page_insert.summary.hot_pages",
            index = &i.name,
            waits = waits,
            time = time,
            inserts = format::count(c.leaf_inserts.unwrap_or(0.0))
        );
    }
    f.severity = latch.severity;
    match latch.history {
        Some(CounterAge::Short) => f.caveats.push(t!(
            "check.last_page_insert.caveat.short",
            engine = engine_name(snapshot),
            since = crate::support::counted_span(snapshot)
        )),
        Some(CounterAge::Unknown) => {
            f.caveats.push(t!("check.last_page_insert.caveat.unknown", engine = engine_name(snapshot)))
        }
        None => {}
    }
    severity_score(&mut f);
    f.impact.set_label(t!("check.last_page_insert.impact", time = format::duration(c.page_latch_wait_ms)));
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.last_page_insert.metric.waits"), c.page_latch_waits, Unit::Count),
        Metric::said(key!("check.last_page_insert.metric.time"), c.page_latch_wait_ms, Unit::Ms),
    ];
    if let Some(inserts) = c.leaf_inserts {
        f.metrics.push(Metric::said(culprit_core::key!("check.last_page_insert.metric.inserts"), inserts, Unit::Count));
    }
    f.fix = Some(match c.optimize_for_sequential_key {
        _ if !latch.inserts => Fix { summary: t!("check.last_page_insert.fix.hot_pages"), sql: None, cost: None },
        Some(_) => Fix {
            summary: t!("check.last_page_insert.fix.pace"),
            sql: Some(format!(
                "ALTER INDEX {} ON {} SET (OPTIMIZE_FOR_SEQUENTIAL_KEY = ON);",
                quote(Engine::Mssql, &i.name),
                qualified(Engine::Mssql, &i.schema, &i.table)
            )),
            cost: None,
        },
        None => Fix { summary: t!("check.last_page_insert.fix.before_2019"), sql: None, cost: None },
    });
    f.verify = if latch.inserts && c.optimize_for_sequential_key.is_some() {
        vec![t!("check.last_page_insert.verify.throughput"), t!("check.last_page_insert.verify.waits_continue")]
    } else {
        vec![t!("check.last_page_insert.verify.compare", work = if latch.inserts { "inserts" } else { "changes" })]
    };
    f
}
