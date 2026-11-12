//! Index page-latch waits. The public check ID is stable; aggregate counters do not establish a last-page cause.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::IndexContention;

const MIN_PAGE_LATCH_WAITS: f64 = 1_000.0;
const MIN_PAGE_LATCH_WAIT_MS: f64 = 10_000.0;

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
    decide = index_latch_waits,
    render = index_latch_finding,
    case<'s> = IndexLatchWaits<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Source(culprit_core::source_id::INDEX_CONTENTION)],
);

pub struct IndexLatchWaits<'s> {
    index: &'s IndexStat,
    contention: &'s IndexContention,
    explanation: LatchExplanation<'s>,
    severity: Severity,
    history: Option<CounterAge>,
}

/// This is a hypothesis about the access pattern, never a claim about a page or its holder.
enum LatchExplanation<'s> {
    SequentialInsertCandidate { column: &'s str },
    Unresolved,
}

#[derive(Clone, Copy)]
pub enum CounterAge {
    Short,
    Unknown,
}

fn index_latch_waits(snapshot: &Snapshot) -> Vec<IndexLatchWaits<'_>> {
    snapshot
        .indexes
        .iter()
        .filter_map(|index| {
            let contention = index.contention.as_ref()?;
            if !(contention.page_latch_waits >= MIN_PAGE_LATCH_WAITS
                && contention.page_latch_wait_ms >= MIN_PAGE_LATCH_WAIT_MS)
            {
                return None;
            }
            // Insert and wait counts have different meanings: one insert can wait many times. Their ratio cannot
            // distinguish last-page inserts from updates or other page access. A paced index can still have waits.
            let explanation = match (contention.sequential_key, contention.leaf_inserts, index.columns.first()) {
                (true, Some(inserts), Some(column)) if inserts > 0.0 => {
                    LatchExplanation::SequentialInsertCandidate { column }
                }
                _ => LatchExplanation::Unresolved,
            };
            let rate = counter_rate_per_day_since(snapshot, contention.page_latch_wait_ms, &contention.reset);
            let history = rate.is_none().then(|| match counter_days_since(snapshot, &contention.reset) {
                Some(_) => CounterAge::Short,
                None => CounterAge::Unknown,
            });
            Some(IndexLatchWaits {
                index,
                contention,
                explanation,
                severity: since_start_severity(rate, 600_000.0, 60_000.0),
                history,
            })
        })
        .collect()
}

fn index_latch_finding(snapshot: &Snapshot, latch: IndexLatchWaits<'_>) -> Finding {
    let (index, contention) = (latch.index, latch.contention);
    let table = index_label(index);
    let mut finding = finding(
        "last-page-insert",
        &format!("{table}.{}", index.name),
        Category::Indexes,
        subject(SubjectKind::Index, format!("{table}.{}", index.name)),
    );
    finding.title = t!("check.last_page_insert.title", index = &index.name);
    finding.summary = t!(
        "check.last_page_insert.summary",
        index = &index.name,
        waits = format::count(contention.page_latch_waits),
        time = format::duration(contention.page_latch_wait_ms)
    );
    let pacing_candidate = match latch.explanation {
        LatchExplanation::SequentialInsertCandidate { column } => {
            finding.caveats.push(t!("check.last_page_insert.caveat.insert_candidate", column = column));
            contention.optimize_for_sequential_key == Some(false)
        }
        LatchExplanation::Unresolved => false,
    };
    if contention.optimize_for_sequential_key == Some(true) {
        finding.caveats.push(t!("check.last_page_insert.caveat.already_paced"));
    }
    finding.severity = latch.severity;
    if latch.history.is_some() {
        finding.actionability = culprit_core::Actionability::Information;
    }
    match latch.history {
        Some(CounterAge::Short) => finding.caveats.push(t!(
            "check.last_page_insert.caveat.short",
            engine = engine_name(snapshot),
            since = culprit_core::span_words(counter_days_since(snapshot, &contention.reset).unwrap_or(0.0) * 24.0)
        )),
        Some(CounterAge::Unknown) => {
            finding.caveats.push(t!("check.last_page_insert.caveat.unknown", engine = engine_name(snapshot)))
        }
        None => {}
    }
    severity_score(&mut finding);
    finding
        .impact
        .set_label(t!("check.last_page_insert.impact", time = format::duration(contention.page_latch_wait_ms)));
    finding.confidence = Confidence::Medium;
    finding.metrics = vec![
        Metric::said(key!("check.last_page_insert.metric.waits"), contention.page_latch_waits, Unit::Count),
        Metric::said(key!("check.last_page_insert.metric.time"), contention.page_latch_wait_ms, Unit::Ms),
    ];
    if let Some(inserts) = contention.leaf_inserts {
        finding.metrics.push(Metric::said(key!("check.last_page_insert.metric.inserts"), inserts, Unit::Count));
    }
    finding.fix = Some(Fix {
        summary: if pacing_candidate {
            t!("check.last_page_insert.fix.pace_candidate")
        } else {
            t!("check.last_page_insert.fix.inspect")
        },
        // A candidate cause cannot authorize a setting or key change.
        sql: None,
        cost: None,
    });
    finding.verify = vec![t!("check.last_page_insert.verify.page"), t!("check.last_page_insert.verify.compare")];
    finding
}
