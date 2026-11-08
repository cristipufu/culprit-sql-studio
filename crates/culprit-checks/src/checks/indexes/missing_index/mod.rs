//! The `missing-index` check: Index suggestions recorded by SQL Server's optimizer, merged with each other and checked against existing indexes.

use super::*;
use crate::evidence::Requirement as Evidence;
#[cfg(test)]
pub(crate) use culprit_core::index_filter::same_literal;
pub(crate) use culprit_core::index_filter::{compared_to, filter_equalities, is_literal};
use culprit_core::t;

mod candidates;
mod parameterized;

pub(crate) use candidates::{
    Candidate, LargeIncludes, SCAN_ROWS, asking_queries, candidate_fix, drop_large_includes, fits_index_filter,
    large_includes, merge_candidates, readable_filter, too_small_to_index, writes_per_day,
};
#[cfg(test)]
pub(super) use parameterized::compares_to_parameter;
pub(crate) use parameterized::{Remainder, parameterized_filter};

use candidates::{
    CandidateFix, LastAsked, MEASURED_READS, Remedy, TooSmall, candidate_remedy, candidate_remedy_fix, reads_per_run,
    request_severity, too_small, too_small_finding, wanted_before_window,
};
use parameterized::{LiteralFilter, literal_filter, literal_filter_finding};

pub fn missing_index_info() -> CheckInfo {
    CheckInfo {
        id: "missing-index".into(),
        title: t!("check.missing_index.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql],
        requires: vec![Capability::MissingIndexes],
        description: t!("check.missing_index.info.description"),
    }
}

check!(
    MissingIndexCheck: missing_index_info,
    decide = missing_indexes,
    render = missing_index_finding,
    case<'s> = IndexRequest<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Source(culprit_core::source_id::MISSING_INDEXES)],
);

/// What the missing-index check concludes about one of the optimizer's requests (merged with those it serves).
pub enum IndexRequest<'s> {
    /// A filtered index serves it once the queries write its filter's literal.
    Literal(LiteralFilter<'s>),
    Request(Request<'s>),
}

/// An optimizer request, and what its finding concludes.
pub struct Request<'s> {
    c: Candidate,
    /// The window's queries that asked for it.
    linked: Vec<&'s QueryStat>,
    rows: Option<f64>,
    /// When the optimizer last asked for it, where that is before the window.
    asked: Option<LastAsked>,
    /// The statements behind it stopped (or the requests did): told in the past.
    history: bool,
    /// The linked queries' share of database time.
    impact: Option<QueryImpact>,
    /// Azure SQL's automatic tuning recommends the same index, with its score.
    azure: Option<Option<f64>>,
    severity: Severity,
    score: f64,
    confidence: Confidence,
    /// Where it isn't advice: history, or context.
    actionability: Option<culprit_core::Actionability>,
    inactive: bool,
    last_seen: Option<String>,
    shape: Shape<'s>,
}

/// What a request's finding advises.
enum Shape<'s> {
    /// The table is too small for an index to pay.
    TooSmall(TooSmall<'s>),
    /// Asked for only before the window: context, not advice.
    Previous,
    /// The statements that wanted it stopped running: nothing to create unless they come back, and the writes an index
    /// would add to.
    Stopped { writes: Option<(f64, f64)> },
    /// An index to create, widen or check: the included columns left out for their size, and how many more the
    /// optimizer suggested than an index should carry.
    Advice { large: Option<LargeIncludes>, wide: Option<usize>, remedy: Remedy<'s> },
}

fn missing_indexes(snapshot: &Snapshot) -> Vec<IndexRequest<'_>> {
    let queries = snapshot.queries();
    merge_candidates(snapshot).into_iter().map(|c| decide(snapshot, queries, c)).collect()
}

fn decide<'s>(snapshot: &'s Snapshot, queries: &'s [QueryStat], mut c: Candidate) -> IndexRequest<'s> {
    let keys = c.keys();
    let linked = asking_queries(queries, &c);
    if let Some(miss) = parameterized_filter(snapshot, &c, &linked) {
        return IndexRequest::Literal(literal_filter(snapshot, c, linked, miss));
    }
    let rows = snapshot.table(&c.schema, &c.table).and_then(|t| t.rows);
    let lookups = c.seeks + c.scans;
    // Asked for only before the window: history once that is a day back, otherwise said and kept active.
    let asked = wanted_before_window(snapshot, &c);
    // Asked for by statements that all stopped running: told in the past, as refine then marks it.
    let history = crate::all_stopped(snapshot, &linked) || asked.as_ref().is_some_and(LastAsked::stopped);
    let impact = query_impact(snapshot, &linked);
    let mut severity = request_severity(snapshot, &c, &linked);
    let (mut score, mut confidence) = match &impact {
        Some(impact) => {
            // What the index saves is the queries' time times the optimizer's estimate of the gain: 11% of a query at
            // 0.01% of database time is nothing to rank or raise above Low.
            let gain = (c.impact_pct / 100.0).clamp(0.0, 1.0);
            // How sure, not how much (the severity says that): the optimizer's estimate is a share of what its
            // queries read. Where their runs measured real reading ([`MEASURED_READS`] pages a run) of a table of
            // SCAN_ROWS rows or more, a large gain is certain; on a small table, or runs that read a few pages, a
            // large share of very little is a guess.
            let measured = reads_per_run(&linked).is_some_and(|pages| pages >= MEASURED_READS);
            let confidence = if c.impact_pct >= 50.0 && measured && rows.is_some_and(|r| r >= SCAN_ROWS) {
                Confidence::High
            } else {
                Confidence::Medium
            };
            (score_for_share(impact.share * gain), confidence)
        }
        None => (
            (6.0 * (c.weight + 1.0).log10()).clamp(4.0, 40.0),
            if lookups >= 1000.0 { Confidence::Medium } else { Confidence::Low },
        ),
    };
    // Azure SQL weighs the same index over a longer history than the optimizer's counters, which reset on restart.
    let azure = crate::tuning::recommended_create(snapshot, &c.schema, &c.table, &keys).map(|r| r.score);
    if azure.is_some() {
        confidence = Confidence::High;
    }
    let mut actionability = None;
    let (mut inactive, mut last_seen) = (false, None);
    if let Some(a) = asked.as_ref().filter(|a| a.stopped()) {
        // An old request is history even if an associated query hash still runs with another plan.
        actionability = Some(culprit_core::Actionability::History);
        inactive = crate::all_stopped(snapshot, &linked);
        severity = Severity::Low;
        score *= 0.25;
        last_seen = Some(a.last.clone());
    }
    let shape = if too_small_to_index(snapshot, &c, &linked) {
        actionability = Some(culprit_core::Actionability::Information);
        severity = Severity::Low;
        confidence = Confidence::Medium;
        score *= 0.25;
        Shape::TooSmall(too_small(snapshot, &c, &linked, inactive))
    } else if let Some(a) = asked.as_ref().filter(|_| !inactive) {
        actionability = Some(if a.stopped() {
            culprit_core::Actionability::History
        } else {
            culprit_core::Actionability::Information
        });
        severity = Severity::Low;
        score = base_score(Severity::Low);
        Shape::Previous
    } else if inactive {
        // A request no query in the window made is history: it gets no index of its own.
        Shape::Stopped { writes: measured_writes(snapshot, &c.schema, &c.table).filter(|(writes, _)| *writes > 0.0) }
    } else {
        // Columns to include matter only for an index worth creating.
        let large = large_includes(snapshot, &c.schema, &c.table, &mut c.include);
        let wide = (c.include.len() > MAX_INCLUDE).then_some(c.include.len());
        if wide.is_some() {
            c.include.clear();
        }
        let remedy = candidate_remedy(snapshot, &c, &linked);
        Shape::Advice { large, wide, remedy }
    };
    IndexRequest::Request(Request {
        c,
        linked,
        rows,
        asked,
        history,
        impact,
        azure,
        severity,
        score,
        confidence,
        actionability,
        inactive,
        last_seen,
        shape,
    })
}

fn missing_index_finding(snapshot: &Snapshot, case: IndexRequest<'_>) -> Finding {
    match case {
        IndexRequest::Literal(l) => literal_filter_finding(snapshot, l),
        IndexRequest::Request(r) => request_finding(snapshot, r),
    }
}

/// The finding every request starts from: its id, subject and keys.
pub(super) fn request_base(c: &Candidate) -> Finding {
    let keys = c.keys();
    let table_name = format!("{}.{}", c.schema, c.table);
    let mut f = finding(
        "missing-index",
        &format!("{table_name}({})", keys.join(",")),
        Category::Indexes,
        subject(SubjectKind::Table, &table_name),
    );
    f.facts.index_keys = keys;
    f
}

/// Under a day: none of the window's statements asked for it, too soon to say they stopped.
fn lately_sentence(snapshot: &Snapshot, a: &LastAsked) -> String {
    t!(
        "check.missing_index.lately",
        window = a.window_possessive(snapshot),
        when = crate::queries::when_phrase(&a.last),
        silence = format::duration(a.silence * 1000.0)
    )
}

fn request_finding(snapshot: &Snapshot, r: Request<'_>) -> Finding {
    let c = &r.c;
    let linked = &r.linked;
    let keys = c.keys();
    let table_name = format!("{}.{}", c.schema, c.table);
    let key_text = format::list(&keys);
    let lookups = c.seeks + c.scans;
    let tense = if r.history { "past" } else { "present" };
    let mut f = request_base(c);
    f.title = t!("check.missing_index.title", table = &c.table, keys = &key_text, tense = tense);
    let mut summary = vec![
        t!(
            "check.missing_index.summary.asked",
            table = &table_name,
            keys = &key_text,
            lookups = format::count(lookups),
            since = since_phrase(snapshot)
        ),
        t!(
            "check.missing_index.summary.reads_more",
            tense = tense,
            table = &table_name,
            counted = if r.rows.is_some() { "yes" } else { "no" },
            rows = r.rows.map(format::count).unwrap_or_default()
        ),
        t!("check.missing_index.summary.estimate", tense = tense, percent = format!("{:.0}", c.impact_pct)),
    ];
    match &r.impact {
        Some(impact) => {
            summary.push(t!(
                "check.missing_index.summary.share",
                n = linked.len().to_string(),
                tense = tense,
                share = format::percent(impact.share)
            ));
            apply_query_impact(&mut f, impact);
        }
        None => {
            if linked.is_empty() && snapshot.workload.is_some() {
                summary.push(match r.asked.as_ref().filter(|a| !a.stopped()) {
                    Some(a) => lately_sentence(snapshot, a),
                    None => t!("check.missing_index.summary.no_query"),
                });
            }
            f.impact.label = t!("check.missing_index.impact.lookups", lookups = format::count(lookups));
        }
    }
    if let Some(score) = r.azure {
        summary.push(t!(
            "check.missing_index.summary.azure",
            scored = if score.is_some() { "yes" } else { "no" },
            score = score.map(|s| format!("{s:.0}")).unwrap_or_default()
        ));
    }
    f.summary = summary.join(" ");
    f.severity = r.severity;
    f.impact.score = r.score;
    f.confidence = r.confidence;
    if let Some(actionability) = r.actionability {
        f.actionability = actionability;
    }
    f.inactive = r.inactive;
    f.last_seen = r.last_seen.clone();
    f.queries = query_refs(linked);
    // A request made before the window is context: no share of the window's time is its.
    if let Some(impact) = r.impact.as_ref().filter(|_| !matches!(r.shape, Shape::Previous)) {
        f.metrics.push(Metric::new(t!("common.metric.share_of_db_time"), impact.share, Unit::Ratio));
    }
    f.metrics.push(Metric::new(t!("check.missing_index.metric.lookups"), lookups, Unit::Count));
    f.metrics.push(Metric::new(t!("check.missing_index.metric.improvement"), c.impact_pct / 100.0, Unit::Ratio));
    if let Some(rows) = r.rows {
        f.metrics.push(Metric::new(t!("common.metric.rows_in_table"), rows, Unit::Rows));
    }
    match &r.asked {
        Some(a) if a.stopped() => f.caveats.push(t!(
            "check.missing_index.caveat.last_asked",
            date = format::friendly_date(&a.last, &snapshot.taken_at),
            silence = format::duration(a.silence * 1000.0)
        )),
        // Said in the summary when no query of the window is linked to it; beside linked ones, as a caveat.
        Some(a) if !linked.is_empty() => f.caveats.push(lately_sentence(snapshot, a)),
        _ => {}
    }
    match &r.shape {
        Shape::TooSmall(small) => too_small_finding(snapshot, &mut f, c, linked, small),
        Shape::Previous => {
            let a = r.asked.as_ref().expect("a request made before the window");
            f.title = t!("check.missing_index.title.previous", table = &c.table, keys = &key_text);
            f.summary = t!(
                "check.missing_index.summary.previous",
                when = crate::queries::when_phrase(&a.last),
                window = a.window(snapshot),
                lookups = format::count(lookups),
                since = since_phrase(snapshot)
            );
            f.impact.db_time_share = None;
            f.impact.time_ms_per_day = None;
            f.impact.label = t!("check.missing_index.impact.previous");
            for q in &mut f.queries {
                q.note = Some(t!("check.missing_index.query_note.previous"));
            }
            f.fix = Some(Fix { summary: t!("check.missing_index.fix.previous"), sql: None, cost: None });
            f.verify = vec![t!("check.missing_index.verify.previous")];
        }
        Shape::Stopped { writes } => {
            f.fix = Some(Fix {
                summary: match writes {
                    Some((writes, days)) => t!(
                        "check.missing_index.fix.stopped_written",
                        keys = &key_text,
                        table = &table_name,
                        how_many = how_many(snapshot, *writes, *days, culprit_core::key!("common.noun.write"))
                    ),
                    None => t!("check.missing_index.fix.stopped"),
                },
                sql: None,
                cost: None,
            });
            f.verify = vec![t!("check.missing_index.verify.stopped")];
            f.facts.nothing_to_check = Some(culprit_core::NothingToCheck::UntilItReturns);
        }
        Shape::Advice { large, wide, remedy } => {
            if let Some(large) = large {
                f.caveats.push(large.note(&c.schema, &c.table));
            }
            if let Some(more) = wide {
                f.caveats.push(t!("check.missing_index.caveat.wide_include", count = more.to_string()));
            }
            let CandidateFix { fix, widens } = candidate_remedy_fix(snapshot, c, linked, remedy);
            // A widen rebuilds an existing index under its own name: the queries seek that one, not a new one.
            f.verify = vec![match (&widens, fix.sql.is_some()) {
                (Some(name), true) => t!("check.missing_index.verify.rebuilt", index = name),
                (Some(name), false) => t!("check.missing_index.verify.existing", index = name),
                (None, _) => t!("check.missing_index.verify.candidate"),
            }];
            if !linked.is_empty() {
                f.verify.push(match &widens {
                    Some(name) => t!("check.missing_index.verify.queries_use", index = name),
                    None => t!("check.missing_index.verify.queries_use_new"),
                });
            }
            f.fix = Some(fix);
            f.caveats.push(t!("check.missing_index.caveat.per_query"));
        }
    }
    f
}
