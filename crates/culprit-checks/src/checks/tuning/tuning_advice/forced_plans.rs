//! Forced plan inspection: scans in forced plans that an existing index could seek, active forced plans summary, and
//! forcing failure reason hints.

use super::*;
use crate::indexes::SCAN_ROWS;

/// A scan in a query's forced plan of a table an existing index could seek, the way the scan filters it.
pub(super) struct SeekableScan<'a> {
    node: u32,
    schema: String,
    table: String,
    columns: Vec<String>,
    rows: f64,
    index: &'a IndexStat,
    /// The share of the rows read that the scan keeps.
    kept: f64,
    /// Columns the scan returns that the index doesn't hold, so a seek would look each row up in the table; empty when
    /// it covers them, `None` when the plan doesn't say which columns it returns (counted as lookups).
    lookups: Option<Vec<String>>,
}

/// An index that doesn't hold every column the query reads costs a lookup per row it finds: worth it over a scan only
/// for a filter far narrower than an index's usual bar.
const LOOKUPS_KEEP_AT_MOST: f64 = 0.01;

pub(super) fn seekable_scan<'a>(snapshot: &'a Snapshot, q: &QueryStat, plan_id: &str) -> Option<SeekableScan<'a>> {
    // The captured plan is the one that took the most time: the forced one, once the forcing has taken hold.
    let dominant = q.workload_plans().iter().max_by(|a, b| a.total_time_ms.total_cmp(&b.total_time_ms))?;
    if dominant.id != plan_id {
        return None;
    }
    q.plan.as_ref()?.nodes().into_iter().find_map(|node| {
        if node.kind != NodeKind::Scan {
            return None;
        }
        let (schema, table) = split_object(node.object.as_deref()?)?;
        let rows = crate::indexes::scan_rows(snapshot, node, &schema, &table)?;
        if rows < SCAN_ROWS {
            return None;
        }
        // The bar every index claim uses (`PlanNode::kept_share`): a scan that keeps much of what it reads is the right
        // plan, forced or not. Rows read as `scan_rows` counts them: a whole table, unless the index is filtered.
        let kept =
            (node.act_rows.unwrap_or_else(|| node.est_rows_total()) / (rows * node.est_executions.max(1.0))).min(1.0);
        if kept > culprit_core::INDEX_KEEPS_AT_MOST {
            return None;
        }
        let cols = predicate_columns(node.predicate.as_deref()?);
        let mut columns = cols.equality.clone();
        columns.extend(cols.range.into_iter().take(1));
        if columns.is_empty() {
            return None;
        }
        let index = leading_index(snapshot, &schema, &table, &cols.equality, &columns)?;
        // Without the plan's output columns, coverage is unknown: count it as lookups.
        let lookups = node.facts().output_columns.as_ref().map(|list| uncovered(snapshot, index, list));
        if lookups.as_ref().is_none_or(|l| !l.is_empty()) && kept > LOOKUPS_KEEP_AT_MOST {
            return None;
        }
        Some(SeekableScan { node: node.id, schema, table, columns, rows, index, kept, lookups })
    })
}

/// Columns of a plan's output list the index doesn't hold: not a key, not included, and not the clustered key every
/// nonclustered index carries.
pub(super) fn uncovered(snapshot: &Snapshot, index: &IndexStat, output: &[String]) -> Vec<String> {
    if index.is_clustered {
        return Vec::new();
    }
    let clustered = snapshot.indexes_on(&index.schema, &index.table).find(|i| i.is_clustered);
    let held: Vec<String> = index
        .key_names()
        .into_iter()
        .map(str::to_string)
        .chain(index.include.iter().cloned())
        .chain(clustered.map(|c| c.key_names().into_iter().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    output
        .iter()
        .filter(|c| !c.is_empty() && !c.starts_with("Expr") && !held.contains(&c.to_ascii_lowercase()))
        .cloned()
        .collect()
}

/// Who forced a plan, as Query Store and the tuning recommendations say.
#[derive(Clone, Copy)]
pub(super) enum Forcing<'s> {
    /// Automatic tuning (automatic plan correction), on the date it applied its recommendation where known.
    Automatic(Option<&'s str>),
    /// Someone, by hand.
    Hand,
    /// Query Store doesn't say.
    Unknown,
}

impl Forcing<'_> {
    /// The `how` a message selects on, and the date for `automatic_on`.
    fn said(self, snapshot: &Snapshot) -> (&'static str, String) {
        match self {
            Forcing::Automatic(Some(at)) => ("automatic_on", format::friendly_date(at, &snapshot.taken_at)),
            Forcing::Automatic(None) => ("automatic", String::new()),
            Forcing::Hand => ("hand", String::new()),
            Forcing::Unknown => ("unknown", String::new()),
        }
    }
}

/// A forced plan that scans where an index could seek, as it stands.
pub struct ForcedScan<'s> {
    plan: &'s ForcedPlan,
    query: &'s QueryStat,
    how: Forcing<'s>,
    scan: SeekableScan<'s>,
    /// The query's time in the window, where it has some.
    impact: Option<QueryImpact>,
    severity: Severity,
    /// When the index first showed, where something dates it as new and not to before the forcing.
    newer: Option<&'s str>,
}

impl<'s> ForcedScan<'s> {
    pub(super) fn of(
        snapshot: &'s Snapshot,
        plan: &'s ForcedPlan,
        query: &'s QueryStat,
        how: Forcing<'s>,
        forced_at: Option<&str>,
        scan: SeekableScan<'s>,
    ) -> ForcedScan<'s> {
        // "Forced before the index existed" only where something dates the index as new (a scan saw it appear, or
        // the database dated a change to its table), and not to before the forcing. SQL Server doesn't say which time
        // zone the tuning view's times are in, so within the widest UTC offset (14 hours) of each other their order is
        // unknown.
        const ZONE_UNKNOWN_SECONDS: f64 = 14.0 * 3600.0;
        let before_forcing = |at: &str| match (parse_epoch_seconds(at), forced_at.and_then(parse_epoch_seconds)) {
            (Some(created), Some(forced)) => created <= forced + ZONE_UNKNOWN_SECONDS,
            _ => false,
        };
        let impact = query_impact(snapshot, &[query]);
        ForcedScan {
            plan,
            query,
            how,
            newer: scan.index.created_at_latest.as_deref().filter(|at| !before_forcing(at)),
            scan,
            severity: match &impact {
                Some(impact) => severity_for_impact(snapshot, &[query], impact),
                None => Severity::Medium,
            },
            impact,
        }
    }
}

pub(super) fn forced_scan_finding(snapshot: &Snapshot, forced: ForcedScan<'_>) -> Finding {
    let ForcedScan { plan: p, query: q, how, scan, newer, .. } = forced;
    let table_name = format!("{}.{}", scan.schema, scan.table);
    let index = &scan.index.name;
    let mut f = finding(
        "tuning-advice",
        &format!("forced-scan:{}", p.query_id),
        Category::Queries,
        subject(SubjectKind::Query, p.query_id.clone()),
    );
    f.title =
        t!("check.tuning_advice.forced_scan.title", statement = query_handle(q), table = &scan.table, index = index);
    let (how, forced_on) = how.said(snapshot);
    let mut summary = vec![t!(
        "check.tuning_advice.forced_scan.summary",
        plan = &p.plan_id,
        how = how,
        date = forced_on,
        query = &p.query_id,
        table = &table_name,
        rows = format::count(scan.rows),
        kept = format::percent(scan.kept),
        columns = format::list(&scan.columns),
        index = index,
        n = scan.columns.len()
    )];
    let lookups = match &scan.lookups {
        Some(columns) if columns.is_empty() => None,
        Some(columns) => Some(format::list(columns)),
        None => Some(t!("check.tuning_advice.forced_scan.columns_it_returns")),
    };
    if let Some(columns) = lookups {
        summary.push(t!(
            "check.tuning_advice.forced_scan.lookups",
            index = index,
            columns = columns,
            kept = format::percent(scan.kept)
        ));
    }
    if let Some(at) = newer {
        summary.push(t!(
            "check.tuning_advice.forced_scan.newer",
            index = index,
            date = format::friendly_date(at, &snapshot.taken_at)
        ));
    }
    f.summary = summary.join(" ");
    f.severity = forced.severity;
    match &forced.impact {
        Some(impact) => apply_query_impact(&mut f, impact),
        None => {
            f.impact.score = base_score(f.severity);
            f.impact.set_label(t!("check.tuning_advice.forced_scan.impact"));
        }
    }
    f.confidence = Confidence::Medium;
    f.metrics.push(Metric::said_text(culprit_core::key!("check.tuning_advice.metric.forced_plan"), p.plan_id.clone()));
    f.metrics
        .push(Metric::said_text(culprit_core::key!("check.tuning_advice.metric.index_that_could_seek"), index.clone()));
    f.metrics.push(Metric::said(culprit_core::key!("check.tuning_advice.metric.rows_in_table"), scan.rows, Unit::Rows));
    f.queries = vec![QueryRef {
        id: q.id.clone(),
        plan_node: Some(scan.node),
        note: Some(t!("check.tuning_advice.forced_scan.note", table = &scan.table)),
    }];
    f.last_seen = q.last_seen.clone();
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.forced_scan.fix", index = index),
        sql: Some(format!("EXEC sp_query_store_unforce_plan @query_id = {}, @plan_id = {};", p.query_id, p.plan_id)),
        cost: None,
    });
    f.verify = vec![
        t!("check.tuning_advice.forced_scan.verify", query = &p.query_id, index = index),
        t!(
            "check.tuning_advice.forced_scan.verify_force_again",
            sql = format!("EXEC sp_query_store_force_plan @query_id = {}, @plan_id = {};", p.query_id, p.plan_id)
        ),
    ];
    f
}

/// Plans listed one by one in the forced-plans finding; the rest are counted.
pub(super) const FORCED_LISTED: usize = 12;

/// A forced plan that applies.
pub(super) struct Forced<'s> {
    pub(super) plan: &'s ForcedPlan,
    pub(super) query: Option<&'s QueryStat>,
    pub(super) how: Forcing<'s>,
    /// Not among the window's statements, and not run for a day or more since this.
    pub(super) quiet: Option<&'s str>,
}

/// The forced plans that apply.
pub struct ForcedPlans<'s> {
    pub(super) kept: Vec<Forced<'s>>,
    /// Automatic plan correction is on, so it keeps checking the plans it forced.
    pub(super) correcting: bool,
}

pub(super) fn forced_plans_finding(snapshot: &Snapshot, plans: ForcedPlans<'_>) -> Finding {
    let kept = &plans.kept;
    let n = kept.len();
    let automatic = kept.iter().filter(|p| matches!(p.how, Forcing::Automatic(_))).count();
    let mut f = finding(
        "tuning-advice",
        "forced-plans",
        Category::Queries,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let count = n.to_string();
    f.title = match (n, automatic) {
        (1, 1) => t!("check.tuning_advice.forced_plans.title.one_automatic"),
        (1, _) => t!("check.tuning_advice.forced_plans.title.one"),
        (_, a) if a == n => t!("check.tuning_advice.forced_plans.title.all_automatic", count = &count),
        (_, 0) => t!("check.tuning_advice.forced_plans.title.none_automatic", count = &count),
        (_, a) => {
            t!("check.tuning_advice.forced_plans.title.some_automatic", count = &count, automatic = a.to_string())
        }
    };
    let mut summary = vec![t!("check.tuning_advice.forced_plans.summary", n = n)];
    match (n, automatic) {
        (_, 0) => {}
        (1, _) => summary.push(t!("check.tuning_advice.forced_plans.automatic.it")),
        (_, a) if a == n => summary.push(t!("check.tuning_advice.forced_plans.automatic.all")),
        (_, 1) => summary.push(t!("check.tuning_advice.forced_plans.automatic.one_of")),
        (_, a) => summary.push(t!("check.tuning_advice.forced_plans.automatic.some", count = a.to_string())),
    }
    // Automatic plan correction keeps checking the plans it forced and unforces one that turns out slower, while it is
    // on; a plan forced by hand (or once it's off) stays forced until someone unforces it.
    let checked = if plans.correcting { automatic } else { 0 };
    summary.push(match (n - checked, checked) {
        (0, _) => t!("check.tuning_advice.forced_plans.checked"),
        (_, 0) => t!("check.tuning_advice.forced_plans.unchecked"),
        (1, _) => t!("check.tuning_advice.forced_plans.checked_but_one"),
        (rest, _) => t!("check.tuning_advice.forced_plans.checked_but_some", count = rest.to_string()),
    });
    f.summary = summary.join(" ");
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + (n as f64).min(10.0) * 0.2;
    f.impact.set_label(t!("check.tuning_advice.forced_plans.impact", n = n, count = &count));
    f.confidence = Confidence::High;
    f.metrics.push(Metric::said(culprit_core::key!("check.tuning_advice.metric.forced_plans"), n as f64, Unit::Count));
    if automatic > 0 {
        f.metrics.push(Metric::said(
            culprit_core::key!("check.tuning_advice.metric.forced_by_automatic_tuning"),
            automatic as f64,
            Unit::Count,
        ));
    }
    let mut lines: Vec<String> = kept
        .iter()
        .take(FORCED_LISTED)
        .map(|p| {
            let (how, forced_on) = p.how.said(snapshot);
            t!(
                "check.tuning_advice.forced_plans.line",
                query = &p.plan.query_id,
                named = if p.query.is_some() { "yes" } else { "no" },
                statement = p.query.map(query_handle).unwrap_or_default(),
                plan = &p.plan.plan_id,
                how = how,
                date = forced_on,
                quiet = if p.quiet.is_some() { "yes" } else { "no" },
                since = p.quiet.map(|last| format::friendly_date(last, &snapshot.taken_at)).unwrap_or_default()
            )
        })
        .collect();
    if n > FORCED_LISTED {
        lines.push(t!("check.tuning_advice.forced_plans.more", count = (n - FORCED_LISTED).to_string()));
    }
    f.caveats.push(t!("check.tuning_advice.forced_plans.caveat", plans = lines.join("\n")));
    f.queries = kept
        .iter()
        .filter_map(|p| p.query.map(|q| (p.plan, q)))
        .take(FORCED_LISTED)
        .map(|(p, q)| QueryRef {
            id: q.id.clone(),
            plan_node: None,
            note: Some(t!("check.tuning_advice.forced_plans.note", plan = &p.plan_id)),
        })
        .collect();
    f.informational();
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.forced_plans.fix"),
        sql: Some(
            std::iter::once("-- One at a time, comparing the query's duration before and after".to_string())
                .chain(kept.iter().take(FORCED_LISTED).map(|p| {
                    format!(
                        "-- EXEC sp_query_store_unforce_plan @query_id = {}, @plan_id = {};",
                        p.plan.query_id, p.plan.plan_id
                    )
                }))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        cost: None,
    });
    f.verify = vec![t!("check.tuning_advice.forced_plans.verify")];
    f
}

/// What a forced plan's failure reason says went wrong, where it says something.
#[derive(Clone, Copy)]
pub(super) enum FailureHint {
    /// NO_INDEX: an index the plan uses is gone.
    IndexGone,
    /// NO_PLAN, GENERAL_FAILURE: the plan can't be produced any more.
    PlanImpossible,
    /// TIME_OUT: compiling with it took too long.
    CompileTimeout,
}

impl FailureHint {
    pub(super) fn of(reason: &str) -> Option<FailureHint> {
        let r = reason.to_ascii_uppercase();
        if r.contains("NO_INDEX") {
            Some(FailureHint::IndexGone)
        } else if r.contains("NO_PLAN") || r.contains("GENERAL_FAILURE") {
            Some(FailureHint::PlanImpossible)
        } else if r.contains("TIME_OUT") || r.contains("TIMEOUT") {
            Some(FailureHint::CompileTimeout)
        } else {
            None
        }
    }

    pub(super) fn said(self) -> String {
        match self {
            FailureHint::IndexGone => t!("check.tuning_advice.forcing_fails.hint.index_gone"),
            FailureHint::PlanImpossible => t!("check.tuning_advice.forcing_fails.hint.plan_impossible"),
            FailureHint::CompileTimeout => t!("check.tuning_advice.forcing_fails.hint.compile_timeout"),
        }
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn captured_output_column_names_are_not_split_into_qualifiers_or_csv_fields() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
        let mut index = snapshot.indexes[0].clone();
        index.is_clustered = false;
        index.columns = vec!["id".into()];
        index.include = vec!["odd.column".into(), "a, b".into(), "two words".into()];
        snapshot.indexes = vec![index.clone()];
        assert!(
            uncovered(&snapshot, &index, &["id".into(), "odd.column".into(), "a, b".into(), "two words".into()])
                .is_empty()
        );
        assert_eq!(uncovered(&snapshot, &index, &["uncovered.column".into()]), vec!["uncovered.column"]);
    }
}
