//! Why two plans of the same statement differ, in plain language: the conditions the optimizer saw (estimator version,
//! parameter values, statistics, memory, parallelism, plan kind) rather than the operators it picked. Works on plans
//! from every engine; each scenario only fires when both plans carry the evidence for it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::format;
use crate::model::{Engine, StatisticsInfo};
use crate::plan::{NodeKind, Plan, PlanKind, PlanNode, PlanWarning};
use crate::plan_diff::PlanDiff;
use crate::redact::redact_sql;
use crate::t;

/// Estimates must differ by this factor, and the larger reach this many rows, to be a scenario.
const ESTIMATE_FACTOR: f64 = 10.0;
const ESTIMATE_MIN_ROWS: f64 = 100.0;
/// Memory grants must differ by this factor, and the larger reach this many KB.
const MEMORY_FACTOR: f64 = 4.0;
const MEMORY_MIN_KB: f64 = 10.0 * 1024.0;
/// Statistics this far behind their table's data are worth naming.
const STALE_SHARE: f64 = 0.1;
const MAX_ESTIMATES: usize = 3;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ScenarioKind {
    /// The plans are for different statement text.
    Statement,
    /// One plan is estimated, the other actual (or generic).
    PlanKind,
    /// SQL Server's cardinality estimator version differs (compatibility level or hint).
    Estimator,
    /// The plans were compiled for different parameter values.
    Parameters,
    /// Row estimates differ between the plans, or from what actually happened.
    Estimates,
    /// The two plans read the same table through different indexes.
    Index,
    /// One plan converts a column's type before comparing it, so it can't seek an index on it.
    Conversion,
    /// One plan runs in parallel and the other doesn't.
    Parallelism,
    /// Memory grants differ a lot, or one plan spills.
    Memory,
}

/// One condition that differs between the plans, with what it does to the plan.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct Scenario {
    pub kind: ScenarioKind,
    pub title: String,
    pub detail: Option<String>,
    /// Node in plan A this is about, when it is about one.
    pub a: Option<u32>,
    pub b: Option<u32>,
}

/// What Culprit knows about the database the plans ran on, from its latest scan: optimizer statistics, so a scenario
/// can say "statistics on orders are stale". Empty when the plans came from files.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default)]
#[ts(export)]
pub struct CompareContext {
    #[serde(default)]
    pub statistics: Vec<StatisticsInfo>,
    /// When the statistics were read (the scan time), so ages are honest.
    #[serde(default)]
    pub taken_at: Option<String>,
}

pub(crate) fn scenarios(
    a: &Plan,
    b: &Plan,
    diff: &PlanDiff,
    name_a: &PlanName<'_>,
    name_b: &PlanName<'_>,
    ctx: &CompareContext,
) -> Vec<Scenario> {
    let mut out = Vec::new();
    let nodes_a: HashMap<u32, &PlanNode> = a.nodes().into_iter().map(|n| (n.id, n)).collect();
    let nodes_b: HashMap<u32, &PlanNode> = b.nodes().into_iter().map(|n| (n.id, n)).collect();
    let pairs: Vec<(&PlanNode, &PlanNode)> =
        diff.matches.iter().filter_map(|m| Some((*nodes_a.get(&m.a)?, *nodes_b.get(&m.b)?))).collect();

    if let (Some(sa), Some(sb)) = (a.statement.as_deref(), b.statement.as_deref())
        && a.engine == b.engine
        && shape(sa, a.engine) != shape(sb, b.engine)
    {
        out.push(Scenario {
            kind: ScenarioKind::Statement,
            title: t!("core.plan_compare.statement.title"),
            detail: Some(t!("core.plan_compare.statement.detail")),
            a: None,
            b: None,
        });
    }

    if a.kind != b.kind {
        let (actual, other) = if a.kind == PlanKind::Actual { (name_a, name_b) } else { (name_b, name_a) };
        let title = if a.kind == PlanKind::Actual || b.kind == PlanKind::Actual {
            t!("core.plan_compare.plan_kind.actual", plan = actual.initial, other = other)
        } else {
            t!(
                "core.plan_compare.plan_kind.kinds",
                plan = name_a.initial,
                kind = kind_word(a.kind),
                other = name_b,
                other_kind = kind_word(b.kind)
            )
        };
        out.push(Scenario {
            kind: ScenarioKind::PlanKind,
            title,
            detail: Some(t!("core.plan_compare.plan_kind.detail")),
            a: None,
            b: None,
        });
    }

    let ce = |p: &Plan| p.facts().cardinality_estimator.map(|v| v.to_string());
    if let (Some(x), Some(y)) = (ce(a), ce(b))
        && x != y
    {
        out.push(Scenario {
            kind: ScenarioKind::Estimator,
            title: t!(
                "core.plan_compare.estimator.title",
                version = x,
                plan = name_a,
                other_version = y,
                other = name_b
            ),
            detail: Some(t!("core.plan_compare.estimator.detail")),
            a: None,
            b: None,
        });
    }

    let params = |p: &Plan| p.facts().compiled_parameters.clone();
    let parameters_differ = matches!((params(a), params(b)), (Some(x), Some(y)) if x != y);
    if let (Some(x), Some(y)) = (params(a), params(b))
        && x != y
    {
        out.push(Scenario {
            kind: ScenarioKind::Parameters,
            title: t!(
                "core.plan_compare.parameters.title",
                plan = name_a,
                values = x,
                other = name_b,
                other_values = y
            ),
            detail: Some(t!("core.plan_compare.parameters.detail")),
            a: None,
            b: None,
        });
    }

    estimate_scenarios(&mut out, &pairs, (a, name_a), (b, name_b), parameters_differ, ctx);

    // Per table, the indexes each plan reads it through (a seek plus a lookup uses two; a scan of the table uses one).
    let mut lines: Vec<(String, String)> = Vec::new();
    for (table_name, (in_a, in_b)) in index_use(a, b) {
        if in_a == in_b {
            continue;
        }
        let uses =
            |list: &[String]| if list.is_empty() { t!("core.plan_compare.index.none") } else { format::list(list) };
        lines.push((
            table_name.clone(),
            t!(
                "core.plan_compare.index.line",
                table = &table_name,
                plan = name_a,
                indexes = uses(&in_a),
                other = name_b,
                other_indexes = uses(&in_b)
            ),
        ));
    }
    if !lines.is_empty() {
        let tables: Vec<String> = lines.iter().map(|(t, _)| t.clone()).collect();
        let mut detail: Vec<String> = lines.into_iter().map(|(_, l)| l).collect();
        detail.push(t!("core.plan_compare.index.detail"));
        out.push(Scenario {
            kind: ScenarioKind::Index,
            title: t!("core.plan_compare.index.title", tables = format::list(&tables)),
            detail: Some(detail.join(" ")),
            a: None,
            b: None,
        });
    }

    let converts = |p: &Plan| {
        p.facts()
            .conversions
            .iter()
            .find(|c| !crate::insights::current_conversions(p, &c.expression).is_empty())
            .map(|c| c.text())
            .or_else(|| {
                p.nodes()
                    .into_iter()
                    .filter(|n| n.warnings.contains(&PlanWarning::ImplicitConversion))
                    .flat_map(|n| [n.predicate.as_ref(), n.seek_predicate.as_ref()])
                    .flatten()
                    .find(|text| !crate::insights::current_conversions(p, text).is_empty())
                    .cloned()
            })
    };
    let (ca, cb) = (converts(a), converts(b));
    if ca.is_some() != cb.is_some() {
        let (who, other, what) = if cb.is_some() { (name_b, name_a, cb) } else { (name_a, name_b, ca) };
        let mut detail = vec![t!("core.plan_compare.conversion.detail")];
        if let Some(w) = what {
            // SQL Server prefixes each issue with its effect ("Seek Plan: …", "Cardinality Estimate: …").
            let expr = w.split_once(": ").filter(|(k, _)| k.len() < 30).map_or(w.as_str(), |(_, e)| e);
            detail.push(t!("core.plan_compare.conversion.here", expression = format::predicate(expr)));
        }
        detail.push(t!("core.plan_compare.conversion.cause"));
        out.push(Scenario {
            kind: ScenarioKind::Conversion,
            title: t!("core.plan_compare.conversion.title", plan = who.initial, other = other),
            detail: Some(detail.join(" ")),
            a: None,
            b: None,
        });
    }

    let (pa, pb) = (parallel(a), parallel(b));
    if pa.is_some() != pb.is_some() {
        let (yes, no, plan_no, dop) =
            if let Some(d) = pa { (name_a, name_b, b, d) } else { (name_b, name_a, a, pb.unwrap_or(0)) };
        let reason = plan_no.facts().parallelism_blocker.as_ref().map(|r| humanize(r));
        out.push(Scenario {
            kind: ScenarioKind::Parallelism,
            title: if dop > 1 {
                t!(
                    "core.plan_compare.parallelism.title_threads",
                    plan = yes.initial,
                    threads = dop.to_string(),
                    other = no
                )
            } else {
                t!("core.plan_compare.parallelism.title", plan = yes.initial, other = no)
            },
            detail: Some(match reason {
                Some(r) => t!("core.plan_compare.parallelism.blocked", plan = no.initial, reason = r),
                None => t!("core.plan_compare.parallelism.detail"),
            }),
            a: None,
            b: None,
        });
    }

    let grant = |p: &Plan| p.facts().memory_granted_kb.or(p.facts().memory_desired_kb);
    if let (Some(ga), Some(gb)) = (grant(a), grant(b))
        && ga.max(gb) >= MEMORY_MIN_KB
        && ratio(ga, gb) >= MEMORY_FACTOR
    {
        let (more, less, big, small) = if gb > ga { (name_b, name_a, gb, ga) } else { (name_a, name_b, ga, gb) };
        out.push(Scenario {
            kind: ScenarioKind::Memory,
            title: t!(
                "core.plan_compare.memory.title",
                plan = more.initial,
                memory = kb(big),
                other = less,
                other_memory = kb(small)
            ),
            detail: Some(t!("core.plan_compare.memory.detail")),
            a: None,
            b: None,
        });
    }
    let spills = |p: &Plan| p.has_warning(PlanWarning::Spill);
    if spills(a) != spills(b) {
        let (yes, no) = if spills(b) { (name_b, name_a) } else { (name_a, name_b) };
        out.push(Scenario {
            kind: ScenarioKind::Memory,
            title: t!("core.plan_compare.spill.title", plan = yes.initial, other = no),
            detail: Some(t!("core.plan_compare.spill.detail")),
            a: None,
            b: None,
        });
    }
    out
}

fn estimate_scenarios(
    out: &mut Vec<Scenario>,
    pairs: &[(&PlanNode, &PlanNode)],
    (a, name_a): (&Plan, &PlanName<'_>),
    (b, name_b): (&Plan, &PlanName<'_>),
    parameters_differ: bool,
    ctx: &CompareContext,
) {
    let mut found: Vec<(f64, Scenario)> = Vec::new();
    // Between the plans: the same step expecting very different row counts.
    for (x, y) in pairs {
        if x.facts().estimated_rows_unavailable || y.facts().estimated_rows_unavailable {
            continue;
        }
        let (ta, tb) = (x.est_rows_total(), y.est_rows_total());
        // A step run once per outer row (a nested loop's inner side) is estimated per execution: two seeks that each
        // expect one row differ only in how often they run, which their outer sides' own estimates say. And a seek
        // per row against one scan of the table expects as many rows overall: a different plan, not another estimate.
        let repeats = x.est_executions > 1.0 || y.est_executions > 1.0;
        let (ea, eb) = if repeats { (x.est_rows, y.est_rows) } else { (ta, tb) };
        if ta.max(tb) < ESTIMATE_MIN_ROWS
            || ratio(ta, tb) < ESTIMATE_FACTOR
            || ratio(ea, eb) < ESTIMATE_FACTOR
            || x.object.is_none()
        {
            continue;
        }
        let why = if parameters_differ {
            Some(t!("core.plan_compare.estimates.parameters"))
        } else {
            stale(ctx, x)
                .map(|s| [s, t!("core.plan_compare.estimates.compiled_around_update")].join(" "))
                .or_else(|| Some(t!("core.plan_compare.estimates.detail")))
        };
        let (expected, other_expected) = (rows(ea), rows(eb));
        found.push((
            ratio(ea, eb),
            Scenario {
                kind: ScenarioKind::Estimates,
                title: if repeats {
                    t!(
                        "core.plan_compare.estimates.title_per_execution",
                        table = table(y),
                        plan = name_a,
                        rows = expected,
                        other = name_b,
                        other_rows = other_expected
                    )
                } else {
                    t!(
                        "core.plan_compare.estimates.title",
                        table = table(y),
                        plan = name_a,
                        rows = expected,
                        other = name_b,
                        other_rows = other_expected
                    )
                },
                detail: why,
                a: Some(x.id),
                b: Some(y.id),
            },
        ));
    }
    // Within an executed plan: estimated vs actual, by the rule behind the Misestimate warning
    // (`PlanNode::misestimates`): the estimate for the runs that happened, and an operator a Limit, a semi join or a
    // merge join stopped early isn't blamed for the rows it never had to read.
    for (plan, name, side_a) in [(a, name_a, true), (b, name_b, false)] {
        let nodes: HashMap<u32, &PlanNode> = plan.nodes().into_iter().map(|n| (n.id, n)).collect();
        for (id, factor) in plan.root.misestimates() {
            let Some(node) = nodes.get(&id) else { continue };
            // Data access only: the operators above repeat the same misestimate.
            let (Some(actual), Some(_)) = (node.act_rows, &node.object) else { continue };
            let estimated = node.est_rows_for_runs();
            let title = t!(
                "core.plan_compare.estimates.misestimate",
                plan = name.initial,
                rows = rows(estimated),
                table = table(node),
                actual = format::count(actual)
            );
            let detail = stale(ctx, node).or_else(|| Some(t!("core.plan_compare.estimates.misestimate_detail")));
            let (ia, ib) = if side_a { (Some(node.id), None) } else { (None, Some(node.id)) };
            found.push((factor.abs(), Scenario { kind: ScenarioKind::Estimates, title, detail, a: ia, b: ib }));
        }
    }
    found.sort_by(|x, y| y.0.total_cmp(&x.0));
    out.extend(found.into_iter().take(MAX_ESTIMATES).map(|(_, s)| s));
}

/// A table, the indexes plan A reads it through and the indexes plan B reads it through.
type IndexUse = (String, (Vec<String>, Vec<String>));

/// How each plan reads the tables both plans read.
fn index_use(a: &Plan, b: &Plan) -> Vec<IndexUse> {
    let collect = |p: &Plan| {
        let mut map: Vec<(String, Vec<String>)> = Vec::new();
        for n in p.nodes() {
            if n.object.is_none() {
                continue;
            }
            let t = table(n);
            let entry = match map.iter_mut().position(|(k, _)| k.eq_ignore_ascii_case(&t)) {
                Some(i) => &mut map[i].1,
                None => {
                    map.push((t, Vec::new()));
                    &mut map.last_mut().expect("just pushed").1
                }
            };
            if let Some(index) = n.index.as_deref() {
                let name = unquote(index);
                if !entry.iter().any(|e| e.eq_ignore_ascii_case(&name)) {
                    entry.push(name);
                }
            }
        }
        map
    };
    let (ma, mb) = (collect(a), collect(b));
    ma.into_iter()
        .filter_map(|(t, mut ia)| {
            let mut ib = mb.iter().find(|(k, _)| k.eq_ignore_ascii_case(&t))?.1.clone();
            ia.sort_by_key(|x| x.to_lowercase());
            ib.sort_by_key(|x| x.to_lowercase());
            let same = ia.len() == ib.len() && ia.iter().zip(&ib).all(|(x, y)| x.eq_ignore_ascii_case(y));
            Some((t, if same { (ia.clone(), ia) } else { (ia, ib) }))
        })
        .collect()
}

/// "Statistics on orders are stale: 23% of the table changed since they were updated, 6 days before the scan." for
/// the node's table, when Culprit knows.
fn stale(ctx: &CompareContext, node: &PlanNode) -> Option<String> {
    let object = node.object.as_deref()?;
    let name = bare(object);
    let schema = object.rsplit('.').nth(1).map(bare);
    let worst = ctx
        .statistics
        .iter()
        .filter(|s| bare(&s.table) == name && schema.as_deref().is_none_or(|sc| bare(&s.schema) == sc))
        .filter_map(|s| Some((s.modifications? / s.rows?.max(1.0), s)))
        .max_by(|x, y| x.0.total_cmp(&y.0))?;
    if worst.0 < STALE_SHARE {
        return None;
    }
    let age = match (&worst.1.last_updated, &ctx.taken_at) {
        (Some(updated), Some(now)) => format::age_between(updated, now),
        _ => None,
    };
    let table = unquote(object.rsplit('.').next().unwrap_or(object));
    let share = format::percent(worst.0.min(9.99));
    Some(match age {
        Some(age) => t!("core.plan_compare.stale_age", table = table, share = share, age = age),
        None => t!("core.plan_compare.stale", table = table, share = share),
    })
}

/// The statement with literals replaced and whitespace and case folded, so the same query with other values compares
/// equal.
fn shape(sql: &str, engine: Engine) -> String {
    redact_sql(sql, engine).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Threads when the plan runs in parallel: SQL Server's DegreeOfParallelism, or parallelism operators (Gather).
fn parallel(plan: &Plan) -> Option<u32> {
    let dop = plan.facts().degree_of_parallelism;
    if let Some(d) = dop {
        return (d > 1).then_some(d);
    }
    plan.nodes().iter().any(|n| n.kind == NodeKind::Parallelism).then_some(0)
}

/// "CouldNotGenerateValidParallelPlan" → "could not generate valid parallel plan".
fn humanize(reason: &str) -> String {
    let mut out = String::new();
    for (i, c) in reason.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

fn rows(n: f64) -> String {
    t!("core.plan_compare.rows", n = n.round(), count = format::count(n))
}

fn kind_word(kind: PlanKind) -> String {
    match kind {
        PlanKind::Estimated => t!("core.plan_compare.kind.estimated"),
        PlanKind::Generic => t!("core.plan_compare.kind.generic"),
        PlanKind::Actual => t!("core.plan_compare.kind.actual"),
    }
}

fn table(node: &PlanNode) -> String {
    let object = node.object.as_deref().unwrap_or("?");
    unquote(object.rsplit('.').next().unwrap_or(object))
}

fn bare(name: &str) -> String {
    unquote(name.rsplit('.').next().unwrap_or(name)).to_lowercase()
}

fn unquote(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '[' | ']' | '"' | '`')).collect()
}

fn ratio(a: f64, b: f64) -> f64 {
    let (a, b) = (a.max(1.0), b.max(1.0));
    if a > b { a / b } else { b / a }
}

fn kb(value: f64) -> String {
    format::bytes(value * 1024.0)
}

/// Names supplied by a person keep their spelling. Built-in names provide both grammatical positions explicitly.
#[derive(Clone, Copy)]
pub(crate) struct PlanName<'a> {
    pub inline: &'a str,
    pub initial: &'a str,
}

impl<'a> PlanName<'a> {
    pub fn supplied(name: &'a str) -> Self {
        Self { inline: name, initial: name }
    }
}

impl From<&PlanName<'_>> for crate::i18n::Arg {
    fn from(value: &PlanName<'_>) -> Self {
        Self::from(value.inline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Engine, Properties};
    use crate::plan_diff::plan_diff_in;

    fn node(
        op: &str,
        kind: NodeKind,
        object: Option<&str>,
        index: Option<&str>,
        est: f64,
        children: Vec<PlanNode>,
    ) -> PlanNode {
        PlanNode {
            facts: Default::default(),
            id: 0,
            op: op.into(),
            kind,
            detail: None,
            object: object.map(str::to_string),
            index: index.map(str::to_string),
            seek_predicate: None,
            predicate: None,
            est_rows: est,
            est_executions: 1.0,
            act_rows: None,
            act_executions: None,
            rows_read: None,
            table_rows: None,
            subtree_cost: 1.0 + children.iter().map(|c| c.subtree_cost).sum::<f64>(),
            self_cost: 0.0,
            act_time_ms: None,
            warnings: Vec::new(),
            props: Properties::new(),
            children,
            runtime: None,
        }
    }

    fn plan(root: PlanNode, facts: crate::PlanFacts) -> Plan {
        let mut plan = Plan {
            facts,
            view: None,
            engine: Engine::Mssql,
            kind: PlanKind::Estimated,
            statement: Some("SELECT * FROM orders WHERE status = @p1".into()),
            total_cost: root.subtree_cost,
            root,
            warnings: Vec::new(),
            index_hints: Vec::new(),
            props: Properties::new(),
            raw: None,
            insights: None,
        };
        fn number(n: &mut PlanNode, next: &mut u32) {
            n.id = *next;
            *next += 1;
            n.children.iter_mut().for_each(|c| number(c, next));
        }
        number(&mut plan.root, &mut 0);
        plan.compute_self_costs();
        plan
    }

    #[test]
    fn database_names_are_ignored_when_lining_plans_up() {
        let a = plan(
            node("Index Seek", NodeKind::Seek, Some("shop_prod.dbo.orders"), Some("ix_status"), 12.0, vec![]),
            crate::PlanFacts::default(),
        );
        let b = plan(
            node("Index Seek", NodeKind::Seek, Some("shop_test.dbo.orders"), Some("ix_status"), 12.0, vec![]),
            crate::PlanFacts::default(),
        );
        let diff = plan_diff_in(&a, "prod", &b, "test", &CompareContext::default());
        assert!(diff.same_shape, "{:#?}", diff.changes);
        assert_eq!(diff.regions.len(), 1);
    }

    #[test]
    fn stale_statistics_and_parameters_explain_different_estimates() {
        let a = plan(
            node("Index Seek", NodeKind::Seek, Some("dbo.orders"), Some("ix_status"), 12.0, vec![]),
            crate::PlanFacts {
                compiled_parameters: Some("@p1 = N'open'".into()),
                cardinality_estimator: Some(150),
                ..Default::default()
            },
        );
        let b = plan(
            node("Clustered Index Scan", NodeKind::Scan, Some("dbo.orders"), Some("pk_orders"), 140_000.0, vec![]),
            crate::PlanFacts {
                compiled_parameters: Some("@p1 = N'open'".into()),
                cardinality_estimator: Some(160),
                ..Default::default()
            },
        );
        let ctx = CompareContext {
            statistics: vec![StatisticsInfo {
                schema: "dbo".into(),
                table: "orders".into(),
                name: "ix_status".into(),
                last_updated: Some("2026-10-05T00:00:00Z".into()),
                rows: Some(1_000_000.0),
                rows_sampled: None,
                modifications: Some(230_000.0),
                auto_created: false,
            }],
            taken_at: Some("2026-10-11T00:00:00Z".into()),
        };
        let diff = plan_diff_in(&a, "Plan 1", &b, "Plan 2", &ctx);
        let kinds: Vec<ScenarioKind> = diff.scenarios.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&ScenarioKind::Estimator), "{kinds:?}");
        assert!(!kinds.contains(&ScenarioKind::Parameters), "same parameters are not a scenario");
        let estimates =
            diff.scenarios.iter().find(|s| s.kind == ScenarioKind::Estimates).expect("an estimates scenario");
        assert_eq!(estimates.title, "On orders, Plan 1 expects 12 rows and Plan 2 expects 140K rows.");
        let detail = estimates.detail.as_deref().unwrap();
        assert!(
            detail.starts_with(
                "Statistics on orders are stale: 23% of the table changed since they were updated, 6.0 days before the \
                 scan."
            ),
            "{detail}"
        );
        assert!(
            diff.scenarios.iter().any(|s| s.kind == ScenarioKind::Index && s.title == "Different indexes on orders.")
        );
    }

    #[test]
    fn different_parameter_values_are_named() {
        let a = plan(
            node("Index Seek", NodeKind::Seek, Some("dbo.orders"), Some("ix_status"), 12.0, vec![]),
            crate::PlanFacts { compiled_parameters: Some("@p1 = N'rare'".into()), ..Default::default() },
        );
        let b = plan(
            node("Clustered Index Scan", NodeKind::Scan, Some("dbo.orders"), Some("pk_orders"), 140_000.0, vec![]),
            crate::PlanFacts { compiled_parameters: Some("@p1 = N'common'".into()), ..Default::default() },
        );
        let diff = plan_diff_in(&a, "plan A", &b, "plan B", &CompareContext::default());
        let p = diff.scenarios.iter().find(|s| s.kind == ScenarioKind::Parameters).expect("parameters");
        assert_eq!(
            p.title,
            "Compiled for different parameter values: plan A for @p1 = N'rare'; plan B for @p1 = N'common'."
        );
        let e = diff.scenarios.iter().find(|s| s.kind == ScenarioKind::Estimates).unwrap();
        assert!(e.detail.as_deref().unwrap().contains("parameter values"));
    }

    // Every scenario, word for word, as two plans and what Culprit knows about their tables raise it: the messages come
    // from locales/en.json.
    #[test]
    fn scenarios_say_why_plans_differ_in_english() {
        let seek = || node("Index Seek", NodeKind::Seek, Some("dbo.orders"), Some("ix_status"), 12.0, vec![]);
        let scan =
            || node("Clustered Index Scan", NodeKind::Scan, Some("dbo.orders"), Some("pk_orders"), 140_000.0, vec![]);
        let facts = crate::PlanFacts::default;
        let stats = |taken_at: Option<&str>| CompareContext {
            statistics: vec![StatisticsInfo {
                schema: "dbo".into(),
                table: "orders".into(),
                name: "ix_status".into(),
                last_updated: Some("2026-10-05T00:00:00Z".into()),
                rows: Some(1_000_000.0),
                rows_sampled: None,
                modifications: Some(230_000.0),
                auto_created: false,
            }],
            taken_at: taken_at.map(str::to_string),
        };
        let kind = |mut p: Plan, kind: PlanKind| {
            p.kind = kind;
            p
        };
        let mut other_statement = plan(seek(), facts());
        other_statement.statement = Some("SELECT * FROM orders WHERE region = @p1".into());
        let mut converts = plan(seek(), facts());
        converts.root.warnings = vec![PlanWarning::ImplicitConversion];
        converts.root.predicate = Some("CONVERT_IMPLICIT(nvarchar(64),[dbo].[orders].[status],0)=[@p1]".into());
        let parallel = |dop: Option<u32>, blocker: Option<&str>| {
            let mut p = plan(seek(), crate::PlanFacts { degree_of_parallelism: dop, ..facts() });
            p.facts_mut().parallelism_blocker = blocker.map(str::to_string);
            p
        };
        let gathered = plan(node("Parallelism", NodeKind::Parallelism, None, None, 12.0, vec![seek()]), facts());
        let granted = |kb: f64| plan(seek(), crate::PlanFacts { memory_granted_kb: Some(kb), ..facts() });
        let misestimated = |rows: f64| {
            let mut root = seek();
            root.act_rows = Some(rows);
            root.act_executions = Some(1.0);
            kind(plan(root, facts()), PlanKind::Actual)
        };
        let ctx = CompareContext::default();
        let cases: Vec<(&str, Plan, Plan, CompareContext)> = vec![
            ("statements", plan(seek(), facts()), other_statement, ctx.clone()),
            ("actual and estimated", kind(plan(seek(), facts()), PlanKind::Actual), plan(seek(), facts()), ctx.clone()),
            ("estimated and actual", plan(seek(), facts()), kind(plan(seek(), facts()), PlanKind::Actual), ctx.clone()),
            (
                "estimated and generic",
                plan(seek(), facts()),
                kind(plan(seek(), facts()), PlanKind::Generic),
                ctx.clone(),
            ),
            (
                "estimator and parameters",
                plan(
                    seek(),
                    crate::PlanFacts {
                        cardinality_estimator: Some(70),
                        compiled_parameters: Some("@p1 = N'rare'".into()),
                        ..facts()
                    },
                ),
                plan(
                    scan(),
                    crate::PlanFacts {
                        cardinality_estimator: Some(160),
                        compiled_parameters: Some("@p1 = N'common'".into()),
                        ..facts()
                    },
                ),
                ctx.clone(),
            ),
            ("stale", plan(seek(), facts()), plan(scan(), facts()), stats(Some("2026-10-11T00:00:00Z"))),
            ("stale, no age", plan(seek(), facts()), plan(scan(), facts()), stats(None)),
            ("estimates", plan(seek(), facts()), plan(scan(), facts()), ctx.clone()),
            ("misestimate", misestimated(40_000.0), misestimated(12.0), ctx.clone()),
            ("misestimate, stale", misestimated(12.0), misestimated(40_000.0), stats(Some("2026-10-11T00:00:00Z"))),
            ("converts", plan(seek(), facts()), converts.clone(), ctx.clone()),
            ("converts no more", converts, plan(seek(), facts()), ctx.clone()),
            (
                "parallel",
                parallel(Some(8), None),
                parallel(Some(1), Some("CouldNotGenerateValidParallelPlan")),
                ctx.clone(),
            ),
            ("serial", parallel(None, None), gathered, ctx.clone()),
            ("memory", granted(40_000.0), granted(1_024.0), ctx.clone()),
            ("memory, less", granted(1_024.0), granted(40_000.0), ctx.clone()),
        ];
        let mut out = Vec::new();
        for (name, a, b, ctx) in cases {
            out.push(format!("## {name}"));
            for s in plan_diff_in(&a, "plan A", &b, "Plan 2", &ctx).scenarios {
                out.push(format!("{:?}: {} | {}", s.kind, s.title, s.detail.as_deref().unwrap_or("-")));
            }
        }
        let got = out.join("\n");
        assert_eq!(
            got,
            r#"## statements
Statement: These plans are for different statements. | Some differences may come from the SQL itself (another filter, join or column), not from the optimizer.
## actual and estimated
PlanKind: plan A is an actual plan and Plan 2 isn't. | Compare estimates with estimates; actual rows and times only exist on the executed plan.
## estimated and actual
PlanKind: Plan 2 is an actual plan and plan A isn't. | Compare estimates with estimates; actual rows and times only exist on the executed plan.
## estimated and generic
PlanKind: plan A is a estimated plan and Plan 2 a generic plan. | Compare estimates with estimates; actual rows and times only exist on the executed plan.
## estimator and parameters
Estimator: Different cardinality estimators: version 70 in plan A, 160 in Plan 2. | SQL Server estimates rows differently per estimator version. It follows the database's compatibility level unless a hint or LEGACY_CARDINALITY_ESTIMATION overrides it, so a compatibility-level change alone can change the plan.
Parameters: Compiled for different parameter values: plan A for @p1 = N'rare'; Plan 2 for @p1 = N'common'. | A plan is optimized for the values it was first compiled with. When some values match a few rows and others many, the same query gets very different plans (parameter sensitivity).
Estimates: On orders, plan A expects 12 rows and Plan 2 expects 140K rows. | The parameter values they were compiled for likely match very different numbers of rows.
Index: Different indexes on orders. | orders: plan A uses ix_status, Plan 2 uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## stale
Estimates: On orders, plan A expects 12 rows and Plan 2 expects 140K rows. | Statistics on orders are stale: 23% of the table changed since they were updated, 6.0 days before the scan. The two plans may have been compiled before and after an update.
Index: Different indexes on orders. | orders: plan A uses ix_status, Plan 2 uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## stale, no age
Estimates: On orders, plan A expects 12 rows and Plan 2 expects 140K rows. | Statistics on orders are stale: 23% of the table changed since they were updated. The two plans may have been compiled before and after an update.
Index: Different indexes on orders. | orders: plan A uses ix_status, Plan 2 uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## estimates
Estimates: On orders, plan A expects 12 rows and Plan 2 expects 140K rows. | Different row estimates lead the optimizer to different joins and access paths. They come from different parameter values, newer statistics or a different estimator.
Index: Different indexes on orders. | orders: plan A uses ix_status, Plan 2 uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## misestimate
Estimates: plan A: estimated 12 rows from orders, got 40K. | Estimates this far off lead to the wrong join type, too little memory or a needless scan.
## misestimate, stale
Estimates: Plan 2: estimated 12 rows from orders, got 40K. | Statistics on orders are stale: 23% of the table changed since they were updated, 6.0 days before the scan.
## converts
Conversion: Plan 2 converts a column's type before comparing it; plan A doesn't. | A converted column can't be searched through its index, so the plan reads every entry instead. Here: CONVERT_IMPLICIT(nvarchar(64),status,0) = @p1. This usually means the application sent the parameter with a different type (e.g. nvarchar for a varchar column).
## converts no more
Conversion: plan A converts a column's type before comparing it; Plan 2 doesn't. | A converted column can't be searched through its index, so the plan reads every entry instead. Here: CONVERT_IMPLICIT(nvarchar(64),status,0) = @p1. This usually means the application sent the parameter with a different type (e.g. nvarchar for a varchar column).
## parallel
Parallelism: plan A runs in parallel (8 threads); Plan 2 runs on one thread. | Plan 2 couldn't go parallel: could not generate valid parallel plan.
## serial
Parallelism: Plan 2 runs in parallel; plan A runs on one thread. | Parallelism helps large scans and joins and costs more CPU; the optimizer only chooses it when the estimated cost passes the server's threshold.
## memory
Memory: plan A reserves 39.1 MB of memory per run; Plan 2 1.0 MB. | Sorts and hashes get memory sized from the row estimates. A large grant on a busy server makes other queries wait for memory; a small one spills to tempdb.
## memory, less
Memory: Plan 2 reserves 39.1 MB of memory per run; plan A 1.0 MB. | Sorts and hashes get memory sized from the row estimates. A large grant on a busy server makes other queries wait for memory; a small one spills to tempdb."#
        );
    }

    fn estimate_titles(a: &Plan, b: &Plan) -> Vec<String> {
        let diff = plan_diff_in(a, "plan A", b, "plan B", &CompareContext::default());
        diff.scenarios.into_iter().filter(|s| s.kind == ScenarioKind::Estimates).map(|s| s.title).collect()
    }

    #[test]
    fn inner_sides_compare_their_estimate_per_execution() {
        // Plan A expects one order and plan B 140K; each seeks one customer per order. The customers seek expects one
        // row per execution in both: only how often it runs differs, and the orders estimates say that.
        let join = |orders: f64, per_order: f64| {
            let mut customers =
                node("Clustered Index Seek", NodeKind::Seek, Some("dbo.customers"), Some("pk"), per_order, vec![]);
            customers.est_executions = orders;
            let outer = node("Index Seek", NodeKind::Seek, Some("dbo.orders"), Some("ix_status"), orders, vec![]);
            plan(
                node("Nested Loops", NodeKind::Join, None, None, orders * per_order, vec![outer, customers]),
                crate::PlanFacts::default(),
            )
        };
        assert_eq!(
            estimate_titles(&join(1.0, 1.0), &join(140_000.0, 1.0)),
            vec!["On orders, plan A expects 1 row and plan B expects 140K rows."]
        );
        // Another estimate per execution is a difference in the customers estimate itself.
        let titles = estimate_titles(&join(100.0, 1.0), &join(100.0, 1_000.0));
        assert_eq!(titles.len(), 1, "{titles:?}");
        assert!(titles[0].starts_with("On customers, plan A expects 1 row and plan B expects"), "{titles:?}");
        assert!(titles[0].ends_with(" per execution."), "{titles:?}");
    }

    #[test]
    fn actual_rows_are_held_to_the_estimate_for_the_runs_that_happened() {
        let ran = |mut n: PlanNode, rows: f64, runs: f64| {
            n.act_rows = Some(rows);
            n.act_executions = Some(runs);
            n
        };
        let actual = |root: PlanNode| {
            let mut p = plan(root, crate::PlanFacts::default());
            p.kind = PlanKind::Actual;
            p
        };
        // SQL Server: the orders seek expected 100 rows and got 10K, so the customers seek ran 10K times where 100 were
        // planned, and found the one row per run it expected. Only orders is misestimated.
        let mut customers =
            node("Clustered Index Seek", NodeKind::Seek, Some("dbo.customers"), Some("pk_customers"), 1.0, vec![]);
        customers.est_executions = 100.0;
        let orders = node("Index Seek", NodeKind::Seek, Some("dbo.orders"), Some("ix_status"), 100.0, vec![]);
        let join = node(
            "Nested Loops",
            NodeKind::Join,
            None,
            None,
            100.0,
            vec![ran(orders, 10_000.0, 1.0), ran(customers, 10_000.0, 10_000.0)],
        );
        let nested = actual(ran(join, 10_000.0, 1.0));
        let titles = estimate_titles(&nested, &nested);
        assert!(!titles.is_empty() && titles.iter().all(|t| t.contains("from orders")), "{titles:?}");

        // PostgreSQL: a scan's estimate is for a full run, and the Limit above stopped it after 10 rows.
        let scan = node("Seq Scan", NodeKind::Scan, Some("public.orders"), None, 5_000_000.0, vec![]);
        let limited =
            actual(ran(node("Limit", NodeKind::Other, None, None, 10.0, vec![ran(scan, 10.0, 1.0)]), 10.0, 1.0));
        assert_eq!(estimate_titles(&limited, &limited), Vec::<String>::new());
    }
}
