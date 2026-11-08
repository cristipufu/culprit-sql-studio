//! Compare two plans for the same statement: which steps changed, where the cost moved, which estimates shifted.
//! Pure, so it works on two Query Store plans, a plan before and after an index, or the same query in two environments.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::format::{self, digits};
use crate::plan::{NodeKind, Plan, PlanNode, PlanWarning};
use crate::plan_compare::{CompareContext, Scenario, cap, scenarios};
use crate::t;

/// A cost-share move smaller than this (10 points of the plan's cost) is noise.
const SHARE_SHIFT: f64 = 0.10;
/// Row counts must differ by this factor, and the larger must reach `ROWS_MIN`, to be worth a line.
const ROWS_FACTOR: f64 = 10.0;
const ROWS_MIN: f64 = 100.0;
/// Total cost or time must differ by this factor before a headline mentions it.
const COST_RATIO: f64 = 1.25;
/// Actual times must differ by this factor and the slower must reach this many milliseconds.
const TIME_FACTOR: f64 = 2.0;
const TIME_MIN_MS: f64 = 10.0;
/// Structural changes touching less than this share of either plan's cost don't make the headlines.
const HEADLINE_WEIGHT: f64 = 0.02;
const MAX_HEADLINES: usize = 3;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ChangeKind {
    /// The same table is read a different way (Index Seek → Index Scan, another index).
    Access,
    /// The same step uses a different operator (Nested Loops → Hash Match).
    Operator,
    /// A step only plan B has.
    Added,
    /// A step only plan A has.
    Removed,
    /// The step's share of the plan's cost, or its actual time, moved a lot.
    Cost,
    /// Estimated or actual rows changed by an order of magnitude.
    Rows,
    /// A plan warning appeared or went away.
    Warning,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PlanChange {
    pub kind: ChangeKind,
    /// Node id in plan A, when the change involves a node there.
    pub a: Option<u32>,
    /// Node id in plan B, when the change involves a node there.
    pub b: Option<u32>,
    pub title: String,
    pub detail: Option<String>,
    /// How much this matters: roughly the share of plan cost involved, with changed data access ranked above other
    /// structural changes. Changes are sorted by it.
    pub weight: f64,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NodeMatch {
    pub a: u32,
    pub b: u32,
}

/// Operators that do the same thing in both plans: the same step on the same table and index, connected to each other.
/// Diagrams tint each region alike on both sides, the way SSMS highlights similar operations.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PlanRegion {
    pub a: Vec<u32>,
    pub b: Vec<u32>,
}

/// Plan structure, measured timings and known optimizer costs. Missing costs are `None`; matching engine cost
/// scales (`comparable`) do not establish that either plan recorded a cost. `matches` pairs corresponding operators,
/// `regions` groups identical parts, and `scenarios` explains conditions that may account for structural differences.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PlanDiff {
    // One to three plain-language sentences, most important first.
    pub headlines: Vec<String>,
    pub cost_a: Option<f64>,
    pub cost_b: Option<f64>,
    // `cost_b / cost_a` when both plans come from the same engine and have a cost.
    pub cost_ratio: Option<f64>,
    // Both root operators' actual time, when both plans were executed.
    pub time_a_ms: Option<f64>,
    pub time_b_ms: Option<f64>,
    // Costs are only comparable within one engine.
    pub comparable: bool,
    // Every operator matched with the same operator, object and index.
    pub same_shape: bool,
    pub changes: Vec<PlanChange>,
    // Operators that correspond across the two plans.
    pub matches: Vec<NodeMatch>,
    pub only_a: Vec<u32>,
    pub only_b: Vec<u32>,
    pub warnings_added: Vec<PlanWarning>,
    pub warnings_removed: Vec<PlanWarning>,
    // Identical parts of the two plans, largest first.
    #[serde(default)]
    pub regions: Vec<PlanRegion>,
    // Conditions that differ between the plans (estimator, parameter values, statistics, memory, parallelism), each in
    // plain language: why the optimizer may have chosen differently.
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
}

/// Result of comparing a statement's current estimated plan against a hypothetical plan with one or more virtual
/// indexes (`CREATE INDEX ...`) in planner memory (`hypopg`).
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct HypotheticalIndexVerification {
    pub baseline: Plan,
    pub hypothetical: Plan,
    pub diff: PlanDiff,
    /// Virtual index names created for the hypothetical plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub virtual_indexes: Vec<String>,
    /// Virtual index names actually chosen by at least one operator in the hypothetical plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub used_virtual_indexes: Vec<String>,
    /// True when the planner chose at least one of the virtual indexes in the hypothetical plan.
    pub uses_virtual_index: bool,
}

/// Compare a baseline estimated plan against a hypothetical estimated plan with virtual indexes, identifying which
/// virtual indexes were chosen and computing the structural [`PlanDiff`].
pub fn verify_hypothetical_index(baseline: Plan, hypothetical: Plan) -> HypotheticalIndexVerification {
    let mut virtual_indexes = hypothetical.facts().virtual_indexes.clone();
    if virtual_indexes.is_empty()
        && let Some(raw) =
            hypothetical.props.get("VirtualIndexes").or_else(|| hypothetical.props.get("virtual_indexes"))
    {
        virtual_indexes = raw.split(", ").map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    }
    let hyp_indexes: Vec<&str> = hypothetical.nodes().into_iter().filter_map(|n| n.index.as_deref()).collect();
    let base_indexes: Vec<&str> = baseline.nodes().into_iter().filter_map(|n| n.index.as_deref()).collect();
    let norm_leaf = |s: &str| normalize(s.rsplit('.').next().unwrap_or(s));
    let same_index_name = |a: &str, b: &str| {
        let (na, nb) = (normalize(a), normalize(b));
        !na.is_empty() && (na == nb || norm_leaf(a) == norm_leaf(b))
    };
    let mut used_virtual_indexes = Vec::new();
    if !virtual_indexes.is_empty() {
        for vi in &virtual_indexes {
            let chosen = hyp_indexes.iter().any(|idx| same_index_name(idx, vi));
            if chosen && !used_virtual_indexes.iter().any(|u: &String| same_index_name(u, vi)) {
                used_virtual_indexes.push(vi.clone());
            }
        }
    } else {
        for idx in hyp_indexes {
            let in_base = base_indexes.iter().any(|b| same_index_name(b, idx));
            if !in_base && !used_virtual_indexes.iter().any(|u: &String| same_index_name(u, idx)) {
                used_virtual_indexes.push(idx.to_string());
            }
        }
    }
    let uses_virtual_index = !used_virtual_indexes.is_empty();
    let diff = plan_diff_named(
        &baseline,
        &t!("core.plan_diff.name.current"),
        &hypothetical,
        &t!("core.plan_diff.name.virtual_index"),
    );
    HypotheticalIndexVerification {
        baseline,
        hypothetical,
        diff,
        virtual_indexes,
        used_virtual_indexes,
        uses_virtual_index,
    }
}

/// Compare plan `b` against plan `a` (the baseline), naming them "plan A" and "plan B" in the headlines.
pub fn plan_diff(a: &Plan, b: &Plan) -> PlanDiff {
    plan_diff_named(a, &t!("core.plan_diff.name.plan_a"), b, &t!("core.plan_diff.name.plan_b"))
}

/// Compare plan `b` against plan `a` (the baseline); headlines refer to them by the given names ("Plan 815", "staging").
pub fn plan_diff_named(a: &Plan, name_a: &str, b: &Plan, name_b: &str) -> PlanDiff {
    plan_diff_in(a, name_a, b, name_b, &CompareContext::default())
}

/// [`plan_diff_named`] with what Culprit knows about the database (statistics from its latest scan), so scenarios can
/// say "statistics on orders are stale".
pub fn plan_diff_in(a: &Plan, name_a: &str, b: &Plan, name_b: &str, context: &CompareContext) -> PlanDiff {
    let fa = Flat::new(a);
    let fb = Flat::new(b);
    let mut m = Matching { a: vec![None; fa.nodes.len()], b: vec![None; fb.nodes.len()] };
    match_anchors(&fa, &fb, &mut m);
    match_bottom_up(&fa, &fb, &mut m, true);
    match_bottom_up(&fa, &fb, &mut m, false);
    match_top_down(&fa, &fb, &mut m);

    let comparable = a.engine == b.engine;
    let comparable_costs = comparable && !a.facts().estimated_cost_unavailable && !b.facts().estimated_cost_unavailable;
    let mut changes = Vec::new();
    let mut same_shape = true;
    for (i, j) in m.a.iter().enumerate().filter_map(|(i, j)| j.map(|j| (i, j))) {
        let (na, nb) = (fa.nodes[i], fb.nodes[j]);
        let (sa, sb) = (fa.share(na), fb.share(nb));
        let reshaped = na.op != nb.op || !same_name(na.index.as_deref(), nb.index.as_deref());
        if reshaped {
            same_shape = false;
            changes.push(reshape_change(na, nb, sa, sb, comparable_costs));
        } else if comparable_costs && (sb - sa).abs() >= SHARE_SHIFT {
            changes.push(PlanChange {
                kind: ChangeKind::Cost,
                a: Some(na.id),
                b: Some(nb.id),
                title: t!(
                    "core.plan_diff.change.cost_share",
                    step = label(nb),
                    before = format::percent(sa),
                    after = format::percent(sb)
                ),
                detail: None,
                weight: (sb - sa).abs(),
            });
        }
        if let (Some(ta), Some(tb)) = (na.act_time_ms, nb.act_time_ms)
            && ta.max(tb) >= TIME_MIN_MS
            && ratio(ta, tb) >= TIME_FACTOR
            && !reshaped
        {
            changes.push(PlanChange {
                kind: ChangeKind::Cost,
                a: Some(na.id),
                b: Some(nb.id),
                title: t!(
                    "core.plan_diff.change.time",
                    step = label(nb),
                    before = format::duration(ta),
                    after = format::duration(tb)
                ),
                detail: Some(t!("core.plan_diff.change.time_detail")),
                weight: (fa.time_share(na) - fb.time_share(nb)).abs().max(0.05),
            });
        }
        if !reshaped || na.object.is_some() {
            rows_change(&mut changes, na, nb, sa.max(sb));
        }
        for w in nb.warnings.iter().filter(|w| !na.warnings.contains(w)) {
            changes.push(warning_change(*w, Some(na.id), Some(nb.id), nb, true, sb));
        }
        for w in na.warnings.iter().filter(|w| !nb.warnings.contains(w)) {
            changes.push(warning_change(*w, Some(na.id), Some(nb.id), nb, false, sa));
        }
    }

    let only_a: Vec<usize> = (0..fa.nodes.len()).filter(|i| m.a[*i].is_none()).collect();
    let only_b: Vec<usize> = (0..fb.nodes.len()).filter(|j| m.b[*j].is_none()).collect();
    same_shape &= only_a.is_empty() && only_b.is_empty();
    // A whole new branch is one change: report its top operator and count the rest.
    for &i in only_a.iter().filter(|&&i| fa.parent[i].is_none_or(|p| m.a[p].is_some())) {
        changes.push(presence_change(ChangeKind::Removed, &fa, i, &m.a));
    }
    for &j in only_b.iter().filter(|&&j| fb.parent[j].is_none_or(|p| m.b[p].is_some())) {
        changes.push(presence_change(ChangeKind::Added, &fb, j, &m.b));
    }

    let warnings_a = all_warnings(a);
    let warnings_b = all_warnings(b);
    let warnings_added: Vec<PlanWarning> = warnings_b.iter().filter(|w| !warnings_a.contains(w)).copied().collect();
    let warnings_removed: Vec<PlanWarning> = warnings_a.iter().filter(|w| !warnings_b.contains(w)).copied().collect();
    for w in b.warnings.iter().filter(|w| !a.warnings.contains(w)) {
        changes.push(plan_warning_change(*w, true));
    }
    for w in a.warnings.iter().filter(|w| !b.warnings.contains(w)) {
        changes.push(plan_warning_change(*w, false));
    }
    changes.sort_by(|x, y| y.weight.total_cmp(&x.weight).then(rank(x.kind).cmp(&rank(y.kind))));

    let cost_ratio =
        (comparable_costs && a.total_cost > 0.0 && b.total_cost > 0.0).then(|| b.total_cost / a.total_cost);
    let (time_a_ms, time_b_ms) = (a.root.act_time_ms, b.root.act_time_ms);
    let mut diff = PlanDiff {
        headlines: Vec::new(),
        cost_a: (!a.facts().estimated_cost_unavailable).then_some(a.total_cost),
        cost_b: (!b.facts().estimated_cost_unavailable).then_some(b.total_cost),
        cost_ratio,
        time_a_ms,
        time_b_ms,
        comparable,
        same_shape,
        changes,
        matches: m
            .a
            .iter()
            .enumerate()
            .filter_map(|(i, j)| j.map(|j| NodeMatch { a: fa.nodes[i].id, b: fb.nodes[j].id }))
            .collect(),
        only_a: only_a.iter().map(|&i| fa.nodes[i].id).collect(),
        only_b: only_b.iter().map(|&j| fb.nodes[j].id).collect(),
        warnings_added,
        warnings_removed,
        regions: regions(&fa, &fb, &m),
        scenarios: Vec::new(),
    };
    diff.headlines = headlines(&diff, a, b, &fa, &fb, name_a, name_b);
    diff.scenarios = scenarios(a, b, &diff, name_a, name_b, context);
    diff
}

/// What operator shares are measured against. Usually the plan's cost, but under a Postgres `Limit` the children's
/// costs assume they run to completion, so their self costs add up to more than the root's early-stop cost.
pub fn plan_cost_basis(plan: &Plan) -> f64 {
    let own: f64 = plan.nodes().iter().map(|n| n.self_cost).sum();
    [plan.total_cost, plan.root.subtree_cost, own].into_iter().fold(0.0, f64::max).max(f64::MIN_POSITIVE)
}

/// Connected groups of matched operators that are identical on both sides (same operator, table and index), where a
/// child joins its parent's group when the parents are matched to each other too.
fn regions(fa: &Flat, fb: &Flat, m: &Matching) -> Vec<PlanRegion> {
    let identical = |i: usize| {
        m.a[i].is_some_and(|j| {
            same_step(fa.nodes[i], fb.nodes[j]) && same_name(fa.nodes[i].index.as_deref(), fb.nodes[j].index.as_deref())
        })
    };
    let mut group: Vec<Option<usize>> = vec![None; fa.nodes.len()];
    let mut out: Vec<PlanRegion> = Vec::new();
    // Pre-order: a parent is grouped before its children.
    for i in 0..fa.nodes.len() {
        if !identical(i) {
            continue;
        }
        let Some(j) = m.a[i] else {
            continue;
        };
        let joined = fa.parent[i].and_then(|p| {
            let same_parents = identical(p) && fb.parent[j].is_some_and(|q| m.a[p] == Some(q));
            if same_parents { group[p] } else { None }
        });
        let g = joined.unwrap_or_else(|| {
            out.push(PlanRegion { a: Vec::new(), b: Vec::new() });
            out.len() - 1
        });
        group[i] = Some(g);
        out[g].a.push(fa.nodes[i].id);
        out[g].b.push(fb.nodes[j].id);
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.a.len()));
    out
}

/// The plan as a pre-order list, so a node's descendants are the `size - 1` entries after it.
struct Flat<'a> {
    nodes: Vec<&'a PlanNode>,
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    size: Vec<usize>,
    total: f64,
    total_time: f64,
}

impl<'a> Flat<'a> {
    fn new(plan: &'a Plan) -> Flat<'a> {
        let mut flat = Flat {
            nodes: Vec::new(),
            parent: Vec::new(),
            children: Vec::new(),
            size: Vec::new(),
            total: plan_cost_basis(plan),
            total_time: plan.root.act_time_ms.filter(|t| *t > 0.0).unwrap_or(1.0),
        };
        flat.push(&plan.root, None);
        flat
    }

    fn push(&mut self, node: &'a PlanNode, parent: Option<usize>) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        self.parent.push(parent);
        self.children.push(Vec::new());
        self.size.push(1);
        for child in &node.children {
            let c = self.push(child, Some(index));
            self.children[index].push(c);
            self.size[index] += self.size[c];
        }
        index
    }

    fn share(&self, node: &PlanNode) -> f64 {
        node.self_cost / self.total
    }

    /// The operator and the unmatched operators under it: a branch only one plan has.
    fn branch(&self, index: usize, matched: &[Option<usize>]) -> Vec<usize> {
        (index..index + self.size[index]).filter(|d| matched[*d].is_none()).collect()
    }

    /// [`Flat::branch`] of the operator with `id`, where `only` lists the operators only this plan has.
    fn branch_of(&self, id: u32, only: &[u32]) -> Vec<usize> {
        let Some(start) = self.nodes.iter().position(|n| n.id == id) else { return Vec::new() };
        (start..start + self.size[start]).filter(|d| only.contains(&self.nodes[*d].id)).collect()
    }

    /// The share of the plan's cost a branch carries.
    fn branch_share(&self, branch: &[usize]) -> f64 {
        branch.iter().map(|d| self.nodes[*d].self_cost).sum::<f64>() / self.total
    }

    fn time_share(&self, node: &PlanNode) -> f64 {
        node.act_time_ms.unwrap_or(0.0) / self.total_time
    }

    fn position(&self, index: usize) -> f64 {
        index as f64 / self.nodes.len().max(1) as f64
    }
}

struct Matching {
    a: Vec<Option<usize>>,
    b: Vec<Option<usize>>,
}

impl Matching {
    fn pair(&mut self, i: usize, j: usize) {
        self.a[i] = Some(j);
        self.b[j] = Some(i);
    }
}

/// Data access is the backbone of a plan: pair the operators that read the same table first.
fn match_anchors(fa: &Flat, fb: &Flat, m: &mut Matching) {
    let mut candidates = Vec::new();
    for (i, na) in fa.nodes.iter().enumerate() {
        let Some(oa) = na.object.as_deref() else { continue };
        for (j, nb) in fb.nodes.iter().enumerate() {
            let Some(ob) = nb.object.as_deref() else { continue };
            if !same_object(oa, ob) {
                continue;
            }
            let mut score = 0.0;
            if same_name(na.index.as_deref(), nb.index.as_deref()) {
                score += 4.0;
            }
            if na.op == nb.op {
                score += 2.0;
            }
            if na.kind == nb.kind {
                score += 1.0;
            }
            score -= (fa.position(i) - fb.position(j)).abs();
            candidates.push((score, i, j));
        }
    }
    candidates.sort_by(|x, y| y.0.total_cmp(&x.0));
    for (_, i, j) in candidates {
        if m.a[i].is_none() && m.b[j].is_none() {
            m.pair(i, j);
        }
    }
}

/// An operator above matched operators corresponds to the operator above their counterparts (joins, sorts, aggregates).
/// The first pass only pairs identical operators, so a Sort can't take the Limit that belongs to the other Limit.
fn match_bottom_up(fa: &Flat, fb: &Flat, m: &mut Matching, same_op: bool) {
    for i in (0..fa.nodes.len()).rev() {
        if m.a[i].is_some() || fa.size[i] == 1 {
            continue;
        }
        let mut common: HashMap<usize, usize> = HashMap::new();
        for d in i + 1..i + fa.size[i] {
            let Some(e) = m.a[d] else { continue };
            let mut x = fb.parent[e];
            while let Some(up) = x {
                if m.b[up].is_none() {
                    *common.entry(up).or_default() += 1;
                }
                x = fb.parent[up];
            }
        }
        let na = fa.nodes[i];
        let best = common
            .into_iter()
            .filter(|(x, _)| !same_op || fb.nodes[*x].op == na.op)
            .map(|(x, shared)| {
                let dice = 2.0 * shared as f64 / ((fa.size[i] - 1) + (fb.size[x] - 1)).max(1) as f64;
                let nb = fb.nodes[x];
                let likeness =
                    f64::from(u8::from(na.op == nb.op)) * 0.2 + f64::from(u8::from(na.kind == nb.kind)) * 0.1;
                (dice + likeness, dice, shared, x)
            })
            .max_by(|x, y| x.0.total_cmp(&y.0).then(y.3.cmp(&x.3)));
        if let Some((_, dice, shared, x)) = best
            && (dice >= 0.4 || (shared >= 1 && na.op == fb.nodes[x].op))
        {
            m.pair(i, x);
        }
    }
}

/// What's left pairs up under matched parents: same operator first, then same kind, then a lone child on each side.
fn match_top_down(fa: &Flat, fb: &Flat, m: &mut Matching) {
    if m.a[0].is_none() && m.b[0].is_none() {
        m.pair(0, 0);
    }
    for i in 0..fa.nodes.len() {
        let Some(j) = m.a[i] else { continue };
        let open_a = |m: &Matching| fa.children[i].iter().copied().filter(|c| m.a[*c].is_none()).collect::<Vec<_>>();
        let open_b = |m: &Matching| fb.children[j].iter().copied().filter(|c| m.b[*c].is_none()).collect::<Vec<_>>();
        let rules: [PairRule; 2] = [same_step, same_kind];
        for rule in rules {
            for c in open_a(m) {
                if let Some(d) = open_b(m).into_iter().find(|d| rule(fa.nodes[c], fb.nodes[*d])) {
                    m.pair(c, d);
                }
            }
        }
        if let ([c], [d]) = (open_a(m).as_slice(), open_b(m).as_slice()) {
            m.pair(*c, *d);
        }
    }
}

type PairRule = fn(&PlanNode, &PlanNode) -> bool;

fn same_step(x: &PlanNode, y: &PlanNode) -> bool {
    x.op == y.op
        && match (x.object.as_deref(), y.object.as_deref()) {
            (Some(a), Some(b)) => same_object(a, b),
            (None, None) => true,
            _ => false,
        }
}

fn same_kind(x: &PlanNode, y: &PlanNode) -> bool {
    x.kind == y.kind && x.kind != NodeKind::Other
}

fn reshape_change(na: &PlanNode, nb: &PlanNode, sa: f64, sb: f64, comparable: bool) -> PlanChange {
    let access = na.object.is_some() && nb.object.is_some();
    let title = if access {
        t!("core.plan_diff.change.access", table = table(nb), before = how(na), after = how(nb))
    } else {
        t!("core.plan_diff.change.operator", before = &na.op, after = &nb.op)
    };
    let detail = (comparable && sa.max(sb) >= 0.01)
        .then(|| t!("core.plan_diff.change.share_detail", a = format::percent(sa), b = format::percent(sb)));
    PlanChange {
        kind: if access { ChangeKind::Access } else { ChangeKind::Operator },
        a: Some(na.id),
        b: Some(nb.id),
        title,
        detail,
        weight: sa.max(sb) + if access { 0.35 } else { 0.2 },
    }
}

fn rows_change(changes: &mut Vec<PlanChange>, na: &PlanNode, nb: &PlanNode, share: f64) {
    let (ea, eb) = (na.est_rows_total(), nb.est_rows_total());
    if !na.facts().estimated_rows_unavailable
        && !nb.facts().estimated_rows_unavailable
        && ea.max(eb) >= ROWS_MIN
        && ratio(ea, eb) >= ROWS_FACTOR
    {
        changes.push(PlanChange {
            kind: ChangeKind::Rows,
            a: Some(na.id),
            b: Some(nb.id),
            title: t!(
                "core.plan_diff.change.estimated_rows",
                step = label(nb),
                before = format::count(ea),
                after = format::count(eb)
            ),
            detail: None,
            weight: share * 0.5 + 0.01,
        });
    }
    if let (Some(ra), Some(rb)) = (na.act_rows, nb.act_rows)
        && ra.max(rb) >= ROWS_MIN
        && ratio(ra, rb) >= ROWS_FACTOR
    {
        changes.push(PlanChange {
            kind: ChangeKind::Rows,
            a: Some(na.id),
            b: Some(nb.id),
            title: t!(
                "core.plan_diff.change.actual_rows",
                step = label(nb),
                before = format::count(ra),
                after = format::count(rb)
            ),
            detail: None,
            weight: share * 0.5 + 0.01,
        });
    }
}

fn warning_change(
    w: PlanWarning,
    a: Option<u32>,
    b: Option<u32>,
    node: &PlanNode,
    added: bool,
    share: f64,
) -> PlanChange {
    PlanChange {
        kind: ChangeKind::Warning,
        a,
        b,
        title: if added {
            t!("core.plan_diff.change.warning", warning = w.label(), step = label(node))
        } else {
            t!("core.plan_diff.change.warning_gone", warning = w.phrase(), step = label(node))
        },
        detail: None,
        weight: share * 0.5 + if added { 0.03 } else { 0.015 },
    }
}

fn plan_warning_change(w: PlanWarning, added: bool) -> PlanChange {
    PlanChange {
        kind: ChangeKind::Warning,
        a: None,
        b: None,
        title: if added {
            t!("core.plan_diff.change.plan_warning", warning = w.label())
        } else {
            t!("core.plan_diff.change.plan_warning_gone", warning = w.phrase())
        },
        detail: None,
        weight: if added { 0.03 } else { 0.015 },
    }
}

fn presence_change(kind: ChangeKind, flat: &Flat, index: usize, matched: &[Option<usize>]) -> PlanChange {
    let node = flat.nodes[index];
    let branch = flat.branch(index, matched);
    let share = flat.branch_share(&branch);
    let steps = branch_words(flat, &branch);
    let (a, b, title) = match kind {
        ChangeKind::Removed => (Some(node.id), None, t!("core.plan_diff.change.only_in_a", steps = steps)),
        _ => (None, Some(node.id), t!("core.plan_diff.change.only_in_b", steps = steps)),
    };
    PlanChange {
        kind,
        a,
        b,
        title,
        detail: Some(t!("core.plan_diff.change.plan_share", share = format::percent(share))),
        weight: share + 0.05,
    }
}

/// The operators of a branch only one plan has: "Sort and Compute Scalar", or the first and how many are below it.
fn branch_words(flat: &Flat, branch: &[usize]) -> String {
    let names: Vec<String> = branch.iter().map(|d| label(flat.nodes[*d])).collect();
    if names.len() <= 3 {
        format::list(&names)
    } else {
        let below = names.len() - 1;
        t!("core.plan_diff.branch", first = &names[0], n = below, count = below.to_string())
    }
}

fn rank(kind: ChangeKind) -> u8 {
    match kind {
        ChangeKind::Access => 0,
        ChangeKind::Operator => 1,
        ChangeKind::Added => 2,
        ChangeKind::Removed => 3,
        ChangeKind::Cost => 4,
        ChangeKind::Warning => 5,
        ChangeKind::Rows => 6,
    }
}

fn headlines(diff: &PlanDiff, a: &Plan, b: &Plan, fa: &Flat, fb: &Flat, name_a: &str, name_b: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !diff.comparable {
        out.push(t!("core.plan_diff.headline.engines", a = a.engine.label(), b = b.engine.label()));
    }
    // Every other sentence opens with plan B's name.
    let plan = cap(name_b);
    let node_a = |id: Option<u32>| id.and_then(|id| fa.nodes.iter().find(|n| n.id == id).copied());
    let node_b = |id: Option<u32>| id.and_then(|id| fb.nodes.iter().find(|n| n.id == id).copied());
    let mut explained: Vec<String> = Vec::new();

    // The lead sentence says how much slower (or faster) B is and, when one change carries it, why.
    let lead = diff.changes.first().and_then(|c| {
        (c.weight >= HEADLINE_WEIGHT && matches!(c.kind, ChangeKind::Access | ChangeKind::Operator))
            .then(|| Some((c, node_a(c.a)?, node_b(c.b)?)))?
    });
    let mut used_lead = false;
    match (magnitude_of(diff), lead) {
        (Some(m), Some((change, x, y))) => {
            let because = if change.kind == ChangeKind::Access {
                explained.push(normalize(&table(x)));
                if m.b_worse {
                    t!(
                        "core.plan_diff.because.access_worse",
                        verb = verb(y, &fb.nodes),
                        share = format::percent(fb.share(y)),
                        other = name_a,
                        other_verb = verb(x, &fa.nodes)
                    )
                } else {
                    t!(
                        "core.plan_diff.because.access_better",
                        verb = verb(y, &fb.nodes),
                        other = name_a,
                        other_verb = verb(x, &fa.nodes),
                        share = format::percent(fa.share(x))
                    )
                }
            } else {
                t!("core.plan_diff.because.operator", op = &y.op, other = name_a, other_op = &x.op)
            };
            let key = match (m.by_time, m.b_worse) {
                (true, true) => crate::key!("core.plan_diff.headline.took_longer_because"),
                (true, false) => crate::key!("core.plan_diff.headline.took_less_time_because"),
                (false, true) => crate::key!("core.plan_diff.headline.more_expensive_because"),
                (false, false) => crate::key!("core.plan_diff.headline.cheaper_because"),
            };
            out.push(crate::i18n::t(
                key,
                &[("plan", plan.as_str().into()), ("factor", m.factor.into()), ("because", because.into())],
            ));
            used_lead = true;
        }
        (Some(m), None) => {
            let key = match (m.by_time, m.b_worse) {
                (true, true) => crate::key!("core.plan_diff.headline.took_longer_than"),
                (true, false) => crate::key!("core.plan_diff.headline.took_less_time_than"),
                (false, true) => crate::key!("core.plan_diff.headline.more_expensive_than"),
                (false, false) => crate::key!("core.plan_diff.headline.cheaper_than"),
            };
            out.push(crate::i18n::t(
                key,
                &[
                    ("plan", plan.as_str().into()),
                    ("factor", m.factor.into()),
                    ("other", name_a.into()),
                    ("before", m.before.into()),
                    ("after", m.after.into()),
                ],
            ));
        }
        _ => {}
    }

    for (index, change) in diff.changes.iter().enumerate().filter(|(_, c)| c.weight >= HEADLINE_WEIGHT) {
        if out.len() >= MAX_HEADLINES {
            break;
        }
        if used_lead && index == 0 {
            continue;
        }
        let sentence = match (change.kind, node_a(change.a), node_b(change.b)) {
            (ChangeKind::Access, Some(x), Some(y)) => {
                explained.push(normalize(&table(x)));
                t!(
                    "core.plan_diff.headline.access",
                    plan = &plan,
                    verb = verb(y, &fb.nodes),
                    other = name_a,
                    other_verb = verb(x, &fa.nodes)
                )
            }
            (ChangeKind::Added | ChangeKind::Removed, x, y)
                if {
                    let (flat, node) = if change.kind == ChangeKind::Added { (fb, y) } else { (fa, x) };
                    let tables = node
                        .map(|n| {
                            branch_tables(
                                flat,
                                n.id,
                                if change.kind == ChangeKind::Added { &diff.only_b } else { &diff.only_a },
                            )
                        })
                        .unwrap_or_default();
                    !tables.is_empty() && tables.iter().all(|t| explained.contains(t))
                } =>
            {
                continue;
            }
            (ChangeKind::Operator, Some(x), Some(y)) if x.kind == NodeKind::Join || y.kind == NodeKind::Join => {
                t!("core.plan_diff.headline.join", plan = &plan, op = &y.op, other = name_a, other_op = &x.op)
            }
            (ChangeKind::Operator, Some(x), Some(y)) => {
                t!("core.plan_diff.headline.operator", plan = &plan, op = &y.op, other = name_a, other_op = &x.op)
            }
            (ChangeKind::Added, _, Some(y)) => {
                let branch = fb.branch_of(y.id, &diff.only_b);
                t!(
                    "core.plan_diff.headline.adds",
                    plan = &plan,
                    initial = initial(&y.op),
                    steps = branch_words(fb, &branch),
                    share = format::percent(fb.branch_share(&branch))
                )
            }
            (ChangeKind::Removed, Some(x), _) => {
                let steps = branch_words(fa, &fa.branch_of(x.id, &diff.only_a));
                t!("core.plan_diff.headline.has_no", plan = &plan, steps = steps, other = name_a)
            }
            _ => continue,
        };
        out.push(sentence);
    }
    if out.len() < MAX_HEADLINES && !diff.warnings_added.is_empty() {
        let names: Vec<String> = diff.warnings_added.iter().map(|w| w.phrase()).collect();
        out.push(t!(
            "core.plan_diff.headline.warnings",
            n = names.len(),
            plan = &plan,
            other = name_a,
            warnings = format::list(&names)
        ));
    }
    if out.is_empty() || (out.len() == 1 && !diff.comparable) {
        out.push(if diff.same_shape {
            t!("core.plan_diff.headline.same_steps", plan = &plan, other = name_a)
        } else {
            t!("core.plan_diff.headline.minor_steps", plan = &plan, other = name_a)
        });
    }
    out
}

/// How much slower or faster B is: by actual time when both plans ran, else by estimated cost.
struct Magnitude {
    b_worse: bool,
    /// Measured by the plans' actual times, rather than their estimated costs.
    by_time: bool,
    /// "3.4×".
    factor: String,
    /// Plan A's and plan B's time ("12 ms") or cost ("0.600").
    before: String,
    after: String,
}

fn magnitude_of(diff: &PlanDiff) -> Option<Magnitude> {
    if let (Some(ta), Some(tb)) = (diff.time_a_ms, diff.time_b_ms)
        && ta > 0.0
        && tb > 0.0
        && ratio(ta, tb) >= COST_RATIO
    {
        return Some(Magnitude {
            b_worse: tb > ta,
            by_time: true,
            factor: times(if tb > ta { tb / ta } else { ta / tb }),
            before: format::duration(ta),
            after: format::duration(tb),
        });
    }
    let r = diff.cost_ratio?;
    if ratio(1.0, r) < COST_RATIO {
        return None;
    }
    Some(Magnitude {
        b_worse: r > 1.0,
        by_time: false,
        factor: times(if r > 1.0 { r } else { 1.0 / r }),
        before: cost(diff.cost_a?),
        after: cost(diff.cost_b?),
    })
}

/// How an operator reads its table, as a verb phrase: "seeks ix_orders_customer", "scans orders". A lookup names the
/// seek on the same table that feeds it.
fn verb(node: &PlanNode, plan: &[&PlanNode]) -> String {
    let table = table(node);
    match node.kind {
        NodeKind::Seek => match node.index.as_deref() {
            Some(index) => t!("core.plan_diff.verb.seeks", index = index),
            None => t!("core.plan_diff.verb.seeks_into", table = table),
        },
        NodeKind::Scan => t!("core.plan_diff.verb.scans", table = table),
        NodeKind::Lookup => {
            let feeder = plan.iter().find(|n| {
                n.kind == NodeKind::Seek && n.index.is_some() && same_name(n.object.as_deref(), node.object.as_deref())
            });
            match feeder.and_then(|n| n.index.as_deref()) {
                Some(index) => t!("core.plan_diff.verb.seeks_and_looks_up", index = index, table = table),
                None => t!("core.plan_diff.verb.looks_up", table = table),
            }
        }
        _ => t!("core.plan_diff.verb.uses_on", op = &node.op, table = table),
    }
}

/// Tables read inside a branch only one plan has (the operator with `id` and the unmatched operators under it).
fn branch_tables(flat: &Flat, id: u32, only: &[u32]) -> Vec<String> {
    let Some(start) = flat.nodes.iter().position(|n| n.id == id) else { return Vec::new() };
    (start..start + flat.size[start])
        .map(|d| flat.nodes[d])
        .filter(|n| only.contains(&n.id) && n.object.is_some())
        .map(|n| normalize(&table(n)))
        .collect()
}

/// "Index Seek (ix_customers_email)" for the change list, where the table is already named.
fn how(node: &PlanNode) -> String {
    match node.index.as_deref() {
        Some(index) => t!("core.plan_diff.step_index", op = &node.op, index = index),
        None => node.op.clone(),
    }
}

/// "Key Lookup on customers", "Sort".
fn label(node: &PlanNode) -> String {
    match node.object.as_deref() {
        Some(_) => t!("core.plan_diff.step_on", op = &node.op, table = table(node)),
        None => node.op.clone(),
    }
}

fn table(node: &PlanNode) -> String {
    let object = node.object.as_deref().unwrap_or("?");
    unquote(object.rsplit('.').next().unwrap_or(object))
}

fn all_warnings(plan: &Plan) -> Vec<PlanWarning> {
    let mut out = Vec::new();
    for w in plan.warnings.iter().chain(plan.nodes().iter().flat_map(|n| n.warnings.iter())) {
        if !out.contains(w) {
            out.push(*w);
        }
    }
    out
}

fn same_object(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    if a == b {
        return true;
    }
    // One side may name the schema and the other not (an imported plan, another tool's output), and database names are
    // ignored so plans from two environments (shop_prod.dbo.orders, shop_test.dbo.orders) line up.
    let (sa, sb) = (a.split('.').collect::<Vec<_>>(), b.split('.').collect::<Vec<_>>());
    if sa.len() == 1 || sb.len() == 1 {
        return sa.last() == sb.last();
    }
    sa[sa.len() - 2..] == sb[sb.len() - 2..]
}

fn same_name(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => normalize(a) == normalize(b),
        (None, None) => true,
        _ => false,
    }
}

fn normalize(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '[' | ']' | '"' | '`')).collect::<String>().to_lowercase()
}

fn unquote(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '[' | ']' | '"' | '`')).collect()
}

fn ratio(a: f64, b: f64) -> f64 {
    let (a, b) = (a.max(1e-9), b.max(1e-9));
    if a > b { a / b } else { b / a }
}

fn times(r: f64) -> String {
    let n = if r >= 10.0 { format::count(r.round()) } else { digits(format!("{r:.1}")) };
    t!("common.factor", n = n)
}

fn cost(value: f64) -> String {
    if value < 10.0 { digits(format!("{value:.3}")) } else { format::count(value) }
}

/// Whether an operator's name (`Index Spool`, `Sort`) starts with a vowel: English says "an" before it. Names are the
/// engine's, in English in every language; each language's message decides what, if anything, this changes.
fn initial(op: &str) -> &'static str {
    if op.chars().next().is_some_and(|c| "AEIOUaeiou".contains(c)) { "vowel" } else { "consonant" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Engine, Properties};
    use crate::plan::PlanKind;

    fn node(
        op: &str,
        kind: NodeKind,
        object: Option<&str>,
        index: Option<&str>,
        cost: f64,
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
            est_rows: 10.0,
            est_executions: 1.0,
            act_rows: None,
            act_executions: None,
            rows_read: None,
            table_rows: None,
            subtree_cost: cost + children.iter().map(|c| c.subtree_cost).sum::<f64>(),
            self_cost: 0.0,
            act_time_ms: None,
            runtime: None,
            warnings: Vec::new(),
            props: Properties::new(),
            children,
        }
    }

    fn plan(engine: Engine, root: PlanNode) -> Plan {
        let mut plan = Plan {
            facts: Default::default(),
            view: None,
            engine,
            kind: PlanKind::Estimated,
            statement: None,
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

    fn seek_plan() -> Plan {
        plan(
            Engine::Mssql,
            node(
                "Nested Loops",
                NodeKind::Join,
                None,
                None,
                0.1,
                vec![
                    node("Index Seek", NodeKind::Seek, Some("shop.orders"), Some("ix_orders_customer"), 0.2, vec![]),
                    node("Key Lookup", NodeKind::Lookup, Some("shop.orders"), Some("pk_orders"), 0.3, vec![]),
                ],
            ),
        )
    }

    /// Everything a comparison says: its headlines, each change and each scenario.
    fn said(diff: &PlanDiff) -> String {
        let mut out = diff.headlines.clone();
        for c in &diff.changes {
            out.push(format!("  {:?}: {} | {}", c.kind, c.title, c.detail.as_deref().unwrap_or("-")));
        }
        for s in &diff.scenarios {
            out.push(format!("  ? {:?}: {} | {}", s.kind, s.title, s.detail.as_deref().unwrap_or("-")));
        }
        out.join("\n")
    }

    /// `plan` with actual times on its root and every step, as an executed plan carries them.
    fn timed(mut p: Plan, ms: f64) -> Plan {
        fn visit(n: &mut PlanNode, ms: f64) {
            n.act_time_ms = Some(ms);
            n.act_rows = Some(n.est_rows);
            n.act_executions = Some(1.0);
            n.children.iter_mut().for_each(|c| visit(c, ms / 2.0));
        }
        visit(&mut p.root, ms);
        p.kind = PlanKind::Actual;
        p
    }

    // What a comparison of two plans says, word for word, for every kind of headline and change: from locales/en.json.
    #[test]
    fn comparisons_say_what_changed_in_english() {
        let mut out = Vec::new();
        let scan = || {
            plan(
                Engine::Mssql,
                node("Clustered Index Scan", NodeKind::Scan, Some("shop.orders"), Some("pk_orders"), 4.0, vec![]),
            )
        };
        let hash = || {
            plan(
                Engine::Mssql,
                node(
                    "Hash Match",
                    NodeKind::Join,
                    None,
                    None,
                    2.0,
                    vec![
                        node("Index Seek", NodeKind::Seek, Some("shop.orders"), Some("ix_orders_customer"), 0.2, vec![]),
                        node("Key Lookup", NodeKind::Lookup, Some("shop.orders"), Some("pk_orders"), 0.3, vec![]),
                    ],
                ),
            )
        };
        let sorted = plan(
            Engine::Mssql,
            node(
                "Sort",
                NodeKind::Sort,
                None,
                None,
                2.0,
                vec![node("Compute Scalar", NodeKind::Compute, None, None, 0.5, vec![seek_plan().root])],
            ),
        );
        let spooled = plan(
            Engine::Mssql,
            node(
                "Index Spool",
                NodeKind::Other,
                None,
                None,
                2.0,
                vec![node(
                    "Eager Spool",
                    NodeKind::Other,
                    None,
                    None,
                    1.0,
                    vec![node(
                        "Filter",
                        NodeKind::Other,
                        None,
                        None,
                        1.0,
                        vec![node("Top", NodeKind::Other, None, None, 1.0, vec![seek_plan().root])],
                    )],
                )],
            ),
        );
        let mut warned = seek_plan();
        warned.root.children[0].warnings = vec![PlanWarning::ImplicitConversion, PlanWarning::Spill];
        warned.warnings = vec![PlanWarning::NoJoinPredicate];
        let mut one_warning = seek_plan();
        one_warning.warnings = vec![PlanWarning::MissingStatistics];
        let mut rows = seek_plan();
        rows.root.children[0].est_rows = 5_000.0;
        let mut acted = timed(seek_plan(), 40.0);
        acted.root.children[0].act_rows = Some(4_000.0);
        let mut cheap = seek_plan();
        cheap.root.children[1].self_cost = 0.0;
        cheap.root.children[0].op = "Index Seek (minor)".into();
        let pg = plan(Engine::Postgres, node("Seq Scan", NodeKind::Scan, Some("public.orders"), None, 50.0, vec![]));
        let sort = |engine: Engine, op: &str, scan: &str| {
            plan(
                engine,
                node(op, NodeKind::Sort, None, None, 2.0, vec![node(scan, NodeKind::Scan, Some("dbo.orders"), None, 5.0, vec![])]),
            )
        };
        for (name, a, b) in [
            ("added, timed", timed(seek_plan(), 10.0), timed(sorted.clone(), 400.0)),
            ("removed, timed", timed(sorted.clone(), 400.0), timed(seek_plan(), 10.0)),
            ("operators", sort(Engine::Mssql, "Sort", "Table Scan"), sort(Engine::Postgres, "Incremental Sort", "Seq Scan")),
            ("same", seek_plan(), seek_plan()),
            ("seek to scan", seek_plan(), scan()),
            ("scan to seek", scan(), seek_plan()),
            ("seek to scan, timed", timed(seek_plan(), 10.0), timed(scan(), 400.0)),
            ("scan to seek, timed", timed(scan(), 400.0), timed(seek_plan(), 10.0)),
            ("join", seek_plan(), hash()),
            ("join, timed", timed(seek_plan(), 10.0), timed(hash(), 30.0)),
            ("added", seek_plan(), sorted.clone()),
            ("removed", sorted.clone(), seek_plan()),
            ("added, many", seek_plan(), spooled.clone()),
            ("removed, many", spooled, seek_plan()),
            ("warnings", seek_plan(), warned.clone()),
            ("warnings gone", warned, seek_plan()),
            ("one warning", seek_plan(), one_warning),
            ("rows", seek_plan(), rows),
            ("actual rows", timed(seek_plan(), 40.0), acted),
            ("minor", seek_plan(), cheap),
            ("engines", seek_plan(), pg),
        ] {
            out.push(format!("## {name}\n{}", said(&plan_diff(&a, &b))));
        }
        let named = plan_diff_named(&seek_plan(), "staging", &scan(), "query 7");
        out.push(format!("## named\n{}", said(&named)));
        let verified = verify_hypothetical_index(scan(), seek_plan());
        out.push(format!("## hypothetical\n{}", said(&verified.diff)));
        // A different shape whose every change is too small to tell.
        let (a, b) = (seek_plan(), seek_plan());
        let mut minor = plan_diff(&a, &b);
        minor.same_shape = false;
        let (fa, fb) = (Flat::new(&a), Flat::new(&b));
        out.push(format!("## minor steps\n{}", headlines(&minor, &a, &b, &fa, &fb, "plan A", "plan B").join("\n")));
        // How each kind of step reads its table.
        for (op, kind, index) in [
            ("Index Seek", NodeKind::Seek, None),
            ("Key Lookup", NodeKind::Lookup, Some("pk")),
            ("Bitmap Heap Scan", NodeKind::Other, None),
        ] {
            out.push(verb(&node(op, kind, Some("dbo.orders"), index, 1.0, vec![]), &[]));
        }
        let got = out.join("\n");
        assert_eq!(got, r#"## added, timed
Plan B took 40× longer than plan A (10 ms → 400 ms).
Plan B adds a Sort and Compute Scalar (81% of its cost).
  Added: Only in B: Sort and Compute Scalar | 81% of that plan's cost
  Cost: Nested Loops: 10 ms → 100 ms | Actual time, across all executions
  Cost: Key Lookup on orders: 50% → 10% of the plan's cost | -
  Cost: Index Seek on orders: 5 ms → 50 ms | Actual time, across all executions
  Cost: Key Lookup on orders: 5 ms → 50 ms | Actual time, across all executions
  Cost: Index Seek on orders: 33% → 6% of the plan's cost | -
  Cost: Nested Loops: 17% → 3% of the plan's cost | -
## removed, timed
Plan B took 40× less time than plan A (400 ms → 10 ms).
Plan B has no Sort and Compute Scalar (plan A has).
  Removed: Only in A: Sort and Compute Scalar | 81% of that plan's cost
  Cost: Nested Loops: 100 ms → 10 ms | Actual time, across all executions
  Cost: Key Lookup on orders: 10% → 50% of the plan's cost | -
  Cost: Index Seek on orders: 50 ms → 5 ms | Actual time, across all executions
  Cost: Key Lookup on orders: 50 ms → 5 ms | Actual time, across all executions
  Cost: Index Seek on orders: 6% → 33% of the plan's cost | -
  Cost: Nested Loops: 3% → 17% of the plan's cost | -
## operators
These plans come from different engines (SQL Server and PostgreSQL), so their costs aren't comparable.
Plan B scans orders where plan A scans orders.
Plan B uses Incremental Sort where plan A uses Sort.
  Access: orders: Table Scan → Seq Scan | -
  Operator: Sort → Incremental Sort | -
## same
Plan B runs the same steps as plan A.
## seek to scan
Plan B is estimated 6.7× more expensive because it scans orders (100% of its cost) where plan A seeks ix_orders_customer and looks up the rows in orders.
  Access: orders: Key Lookup (pk_orders) → Clustered Index Scan (pk_orders) | 50% of the plan's cost in A, 100% in B
  Removed: Only in A: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: plan A uses ix_orders_customer and pk_orders, plan B uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## scan to seek
Plan B is estimated 6.7× cheaper because it seeks ix_orders_customer and looks up the rows in orders where plan A scans orders (100% of plan A's cost).
  Access: orders: Clustered Index Scan (pk_orders) → Key Lookup (pk_orders) | 100% of the plan's cost in A, 50% in B
  Added: Only in B: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: plan A uses pk_orders, plan B uses ix_orders_customer and pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## seek to scan, timed
Plan B took 40× longer because it scans orders (100% of its cost) where plan A seeks ix_orders_customer and looks up the rows in orders.
  Access: orders: Key Lookup (pk_orders) → Clustered Index Scan (pk_orders) | 50% of the plan's cost in A, 100% in B
  Removed: Only in A: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: plan A uses ix_orders_customer and pk_orders, plan B uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## scan to seek, timed
Plan B took 40× less time because it seeks ix_orders_customer and looks up the rows in orders where plan A scans orders (100% of plan A's cost).
  Access: orders: Clustered Index Scan (pk_orders) → Key Lookup (pk_orders) | 100% of the plan's cost in A, 50% in B
  Added: Only in B: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: plan A uses pk_orders, plan B uses ix_orders_customer and pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## join
Plan B is estimated 4.2× more expensive because it uses Hash Match where plan A uses Nested Loops.
  Operator: Nested Loops → Hash Match | 17% of the plan's cost in A, 80% in B
  Cost: Key Lookup on orders: 50% → 12% of the plan's cost | -
  Cost: Index Seek on orders: 33% → 8% of the plan's cost | -
## join, timed
Plan B took 3.0× longer because it uses Hash Match where plan A uses Nested Loops.
  Operator: Nested Loops → Hash Match | 17% of the plan's cost in A, 80% in B
  Cost: Key Lookup on orders: 50% → 12% of the plan's cost | -
  Cost: Index Seek on orders: 33% → 8% of the plan's cost | -
  Cost: Index Seek on orders: 5 ms → 15 ms | Actual time, across all executions
  Cost: Key Lookup on orders: 5 ms → 15 ms | Actual time, across all executions
## added
Plan B is estimated 5.2× more expensive than plan A (cost 0.600 → 3.100).
Plan B adds a Sort and Compute Scalar (81% of its cost).
  Added: Only in B: Sort and Compute Scalar | 81% of that plan's cost
  Cost: Key Lookup on orders: 50% → 10% of the plan's cost | -
  Cost: Index Seek on orders: 33% → 6% of the plan's cost | -
  Cost: Nested Loops: 17% → 3% of the plan's cost | -
## removed
Plan B is estimated 5.2× cheaper than plan A (cost 3.100 → 0.600).
Plan B has no Sort and Compute Scalar (plan A has).
  Removed: Only in A: Sort and Compute Scalar | 81% of that plan's cost
  Cost: Key Lookup on orders: 10% → 50% of the plan's cost | -
  Cost: Index Seek on orders: 6% → 33% of the plan's cost | -
  Cost: Nested Loops: 3% → 17% of the plan's cost | -
## added, many
Plan B is estimated 9.3× more expensive than plan A (cost 0.600 → 5.600).
Plan B adds an Index Spool and 3 operators below it (89% of its cost).
  Added: Only in B: Index Spool and 3 operators below it | 89% of that plan's cost
  Cost: Key Lookup on orders: 50% → 5% of the plan's cost | -
  Cost: Index Seek on orders: 33% → 4% of the plan's cost | -
  Cost: Nested Loops: 17% → 2% of the plan's cost | -
## removed, many
Plan B is estimated 9.3× cheaper than plan A (cost 5.600 → 0.600).
Plan B has no Index Spool and 3 operators below it (plan A has).
  Removed: Only in A: Index Spool and 3 operators below it | 89% of that plan's cost
  Cost: Key Lookup on orders: 5% → 50% of the plan's cost | -
  Cost: Index Seek on orders: 4% → 33% of the plan's cost | -
  Cost: Nested Loops: 2% → 17% of the plan's cost | -
## warnings
Plan B has plan warnings plan A doesn't: no join predicate, implicit conversion and spills to disk.
  Warning: Implicit conversion on Index Seek on orders | -
  Warning: Spills to disk on Index Seek on orders | -
  Warning: Plan warning: No join predicate | -
  ? Memory: Plan B spills to disk; plan A doesn't. | Its sort or hash got less memory than the rows needed, usually because it expected fewer rows than it read.
## warnings gone
Plan B runs the same steps as plan A.
  Warning: No longer: implicit conversion on Index Seek on orders | -
  Warning: No longer: spills to disk on Index Seek on orders | -
  Warning: No longer: no join predicate | -
  ? Memory: Plan A spills to disk; plan B doesn't. | Its sort or hash got less memory than the rows needed, usually because it expected fewer rows than it read.
## one warning
Plan B has a plan warning plan A doesn't: missing statistics.
  Warning: Plan warning: Missing statistics | -
## rows
Plan B runs the same steps as plan A.
  Rows: Index Seek on orders: estimated rows 10 → 5.0K | -
  ? Estimates: On orders, plan A expects 10 rows and plan B expects 5.0K rows. | Different row estimates lead the optimizer to different joins and access paths. They come from different parameter values, newer statistics or a different estimator.
## actual rows
Plan B runs the same steps as plan A.
  Rows: Index Seek on orders: actual rows 10 → 4.0K | -
  ? Estimates: Plan B: estimated 10 rows from orders, got 4.0K. | Estimates this far off lead to the wrong join type, too little memory or a needless scan.
## minor
Plan B seeks ix_orders_customer where plan A seeks ix_orders_customer.
  Access: orders: Index Seek (ix_orders_customer) → Index Seek (minor) (ix_orders_customer) | 33% of the plan's cost in A, 33% in B
  Cost: Key Lookup on orders: 50% → 0% of the plan's cost | -
## engines
These plans come from different engines (SQL Server and PostgreSQL), so their costs aren't comparable.
Plan B joins with Seq Scan where plan A uses Nested Loops.
Plan B has no Key Lookup on orders (plan A has).
  Operator: Nested Loops → Seq Scan | -
  Removed: Only in A: Key Lookup on orders | 50% of that plan's cost
  Removed: Only in A: Index Seek on orders | 33% of that plan's cost
  ? Index: Different indexes on orders. | orders: plan A uses ix_orders_customer and pk_orders, plan B uses no index. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## named
Query 7 is estimated 6.7× more expensive because it scans orders (100% of its cost) where staging seeks ix_orders_customer and looks up the rows in orders.
  Access: orders: Key Lookup (pk_orders) → Clustered Index Scan (pk_orders) | 50% of the plan's cost in A, 100% in B
  Removed: Only in A: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: staging uses ix_orders_customer and pk_orders, query 7 uses pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## hypothetical
With virtual index is estimated 6.7× cheaper because it seeks ix_orders_customer and looks up the rows in orders where Current plan scans orders (100% of Current plan's cost).
  Access: orders: Clustered Index Scan (pk_orders) → Key Lookup (pk_orders) | 100% of the plan's cost in A, 50% in B
  Added: Only in B: Nested Loops and Index Seek on orders | 50% of that plan's cost
  ? Index: Different indexes on orders. | orders: Current plan uses pk_orders, With virtual index uses ix_orders_customer and pk_orders. If an index was added, dropped or rebuilt between the two plans, that explains it; otherwise the optimizer chose differently because it expected a different number of rows.
## minor steps
Plan B differs from plan A only in minor steps.
seeks into orders
looks up rows in orders
uses Bitmap Heap Scan on orders"#);
    }

    #[test]
    fn identical_plans_have_no_changes() {
        let diff = plan_diff(&seek_plan(), &seek_plan());
        assert!(diff.same_shape && diff.changes.is_empty());
        assert_eq!(diff.matches.len(), 3);
        assert_eq!(diff.headlines, ["Plan B runs the same steps as plan A."]);
    }

    #[test]
    fn a_scan_instead_of_a_seek_leads_the_headlines() {
        let scan = plan(
            Engine::Mssql,
            node("Clustered Index Scan", NodeKind::Scan, Some("shop.orders"), Some("pk_orders"), 4.0, vec![]),
        );
        let diff = plan_diff_named(&seek_plan(), "Plan 815", &scan, "Plan 685");
        assert_eq!(
            diff.headlines[0],
            "Plan 685 is estimated 6.7× more expensive because it scans orders (100% of its cost) where Plan 815 seeks \
             ix_orders_customer and looks up the rows in orders."
        );
        assert_eq!(diff.changes[0].kind, ChangeKind::Access);
        assert_eq!(diff.changes[0].title, "orders: Key Lookup (pk_orders) → Clustered Index Scan (pk_orders)");
        assert!(diff.changes.iter().any(|c| c.kind == ChangeKind::Removed && c.title == "Only in A: Nested Loops and Index Seek on orders"));
    }

    #[test]
    fn a_new_branch_is_one_change() {
        let sorted = plan(
            Engine::Mssql,
            node(
                "Sort",
                NodeKind::Sort,
                None,
                None,
                2.0,
                vec![node("Compute Scalar", NodeKind::Compute, None, None, 0.5, vec![seek_plan().root])],
            ),
        );
        let diff = plan_diff(&seek_plan(), &sorted);
        let added: Vec<_> = diff.changes.iter().filter(|c| c.kind == ChangeKind::Added).collect();
        assert_eq!(added.len(), 1, "{:#?}", diff.changes);
        assert_eq!(added[0].title, "Only in B: Sort and Compute Scalar");
        assert!(
            diff.headlines.iter().any(|h| h.starts_with("Plan B adds a Sort and Compute Scalar (")),
            "{:?}",
            diff.headlines
        );
        assert_eq!(diff.only_b.len(), 2);
    }

    #[test]
    fn engines_are_not_compared_by_cost() {
        let pg = plan(Engine::Postgres, node("Seq Scan", NodeKind::Scan, Some("public.orders"), None, 50.0, vec![]));
        let diff = plan_diff(&seek_plan(), &pg);
        assert!(!diff.comparable && diff.cost_ratio.is_none());
        assert!(diff.headlines[0].starts_with("These plans come from different engines (SQL Server and PostgreSQL)"));
    }

    #[test]
    fn missing_retained_estimates_do_not_become_zero_row_or_cost_regressions() {
        let mut cached = plan(
            Engine::Yugabyte,
            node("Index Scan", NodeKind::Seek, Some("public.orders"), Some("orders_pkey"), 0.0, vec![]),
        );
        cached.root.est_rows = 0.0;
        cached.root.facts_mut().estimated_rows_unavailable = true;
        cached.facts_mut().estimated_cost_unavailable = true;
        let mut current = cached.clone();
        current.root.est_rows = 5_000.0;
        current.root.facts_mut().estimated_rows_unavailable = false;
        current.facts_mut().estimated_cost_unavailable = false;
        current.root.self_cost = 100.0;
        current.root.subtree_cost = 100.0;
        current.total_cost = 100.0;
        let diff = plan_diff(&cached, &current);
        assert!(diff.same_shape && diff.comparable && diff.cost_ratio.is_none());
        assert!(diff.cost_a.is_none() && diff.cost_b == Some(100.0));
        assert!(diff.changes.is_empty(), "{:?}", diff.changes);
        assert!(diff.scenarios.iter().all(|s| s.kind != crate::plan_compare::ScenarioKind::Estimates));
    }

    #[test]
    fn shares_under_a_limit_use_the_operators_cost() {
        let mut limited = plan(
            Engine::Postgres,
            node(
                "Limit",
                NodeKind::Other,
                None,
                None,
                0.0,
                vec![node("Seq Scan", NodeKind::Scan, Some("orders"), None, 1000.0, vec![])],
            ),
        );
        limited.root.subtree_cost = 5.0;
        limited.total_cost = 5.0;
        limited.compute_self_costs();
        assert_eq!(plan_cost_basis(&limited), 1000.0);
    }

    #[test]
    fn verify_hypothetical_index_detects_virtual_index_adoption_and_cost_reduction() {
        let baseline =
            plan(Engine::Postgres, node("Seq Scan", NodeKind::Scan, Some("public.orders"), None, 500.0, vec![]));
        let mut hypothetical = plan(
            Engine::Postgres,
            node(
                "Index Scan",
                NodeKind::Seek,
                Some("public.orders"),
                Some("<13542>btree_orders_customer_id"),
                8.5,
                vec![],
            ),
        );
        hypothetical.facts_mut().virtual_indexes = vec!["<13542>btree_orders_customer_id".into()];
        let verified = verify_hypothetical_index(baseline, hypothetical);
        assert!(verified.uses_virtual_index);
        assert_eq!(verified.virtual_indexes, vec!["<13542>btree_orders_customer_id"]);
        assert_eq!(verified.used_virtual_indexes, vec!["<13542>btree_orders_customer_id"]);
        assert!(!verified.diff.same_shape);
        assert!(verified.diff.cost_ratio.is_some_and(|r| r < 0.05));

        // If the planner keeps scanning an existing index whose name is a substring of the virtual index name,
        // `uses_virtual_index` must remain false.
        let base_idx = plan(
            Engine::Postgres,
            node("Index Scan", NodeKind::Seek, Some("public.orders"), Some("orders"), 120.0, vec![]),
        );
        let mut hyp_unchanged = base_idx.clone();
        hyp_unchanged.facts_mut().virtual_indexes = vec!["<13542>btree_orders_customer_id".into()];
        let ignored = verify_hypothetical_index(base_idx.clone(), hyp_unchanged);
        assert!(!ignored.uses_virtual_index);
        assert!(ignored.used_virtual_indexes.is_empty());

        // Schema-qualified or quoted virtual index names match unqualified plan node index names.
        let mut hyp_qualified = plan(
            Engine::Postgres,
            node("Index Scan", NodeKind::Seek, Some("public.orders"), Some("ix_orders_status"), 9.0, vec![]),
        );
        hyp_qualified.facts_mut().virtual_indexes = vec!["public.\"ix_orders_status\"".into()];
        let matched = verify_hypothetical_index(base_idx, hyp_qualified);
        assert!(matched.uses_virtual_index);
        assert_eq!(matched.used_virtual_indexes, vec!["public.\"ix_orders_status\""]);
    }
}
