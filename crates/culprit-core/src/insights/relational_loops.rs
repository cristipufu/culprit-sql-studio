//! One actual TVP-fed loop can establish repeated access without identifying harmful reuse or an optimal join.
use super::{relational_inputs::AccessComparison, *};

/// A repeated single-row seek can be an efficient implementation. Require materially more native access per
/// invocation as well as the existing loop-count policy before diagnosing this particular mechanism.
const MIN_ROWS_PER_INNER_EXECUTION: f64 = 100.0;
const MAX_EXACT_COUNT: u64 = (1 << 53) - 1;

pub(super) fn has_relational_input(node: &PlanNode) -> bool {
    relational_input_kind(node).is_some() || node.children.iter().any(has_relational_input)
}

pub(super) fn tvp_loop(cx: &Cx, join: &PlanNode) -> Option<Draft> {
    ObservedTvpLoop::from_plan(cx, join).map(|observed| observed.render(cx))
}

struct ObservedTvpLoop<'a> {
    join: &'a PlanNode,
    outer: &'a PlanNode,
    inner: &'a PlanNode,
    access: AccessComparison,
    estimated_output: f64,
    actual_output: f64,
    inner_executions: f64,
    inner_accessed: f64,
}

impl<'a> ObservedTvpLoop<'a> {
    fn from_plan(cx: &Cx<'a>, join: &'a PlanNode) -> Option<Self> {
        let [outer, inner] = join.children.as_slice() else { return None };
        if !qualified_tvp_scan(outer) || !direct_inner_access(inner) || !single_serial_path(cx, join, outer) {
            return None;
        }
        let access = AccessComparison::from_node(cx, outer)?;
        let (estimated_output, actual_output) = access.output_counts()?;
        let inner_executions = serial_executions(inner)?;
        let inner_accessed = inner.facts().access_rows.filter(|rows| rows.is_valid())?.actual_total?;
        if !exact_count(actual_output)
            || !exact_count(access.actual_accessed())
            || !exact_count(inner_accessed)
            || actual_output < MISESTIMATE_FACTOR * estimated_output.max(1.0)
            || inner_executions != actual_output
            || inner_executions < MANY_LOOPS
            || inner_accessed / inner_executions < MIN_ROWS_PER_INNER_EXECUTION
            || inner.act_rows.is_none_or(|rows| !exact_count(rows) || rows > inner_accessed)
        {
            return None;
        }
        Some(Self { join, outer, inner, access, estimated_output, actual_output, inner_executions, inner_accessed })
    }

    fn render(self, cx: &Cx) -> Draft {
        let input = what(self.outer);
        let inner = what(self.inner);
        let impact = (cx.subtree(self.inner) + cx.share(self.join)).min(1.0);
        let mut draft =
            Draft::new("nested-loops", InsightArea::Query, impact, vec![self.join.id, self.outer.id, self.inner.id]);
        draft.cap = Severity::Medium;
        draft.title = t!("insight.nested_loops.tvp.title", inner = &inner);
        let mut detail = vec![
            t!(
                "insight.nested_loops.tvp.outer",
                input = &input,
                estimated = rows_text(self.estimated_output),
                actual = rows_text(self.actual_output)
            ),
            self.access.detail(),
            t!(
                "insight.nested_loops.tvp.inner",
                inner = &inner,
                executions = format::count(self.inner_executions),
                accessed = rows_text(self.inner_accessed),
                per_execution = rows_text(self.inner_accessed / self.inner_executions)
            ),
        ];
        if self.join.id != cx.plan.root.id
            && let Some(output) = cx.plan.root.act_rows.filter(|rows| exact_count(*rows))
        {
            detail.push(t!("insight.nested_loops.tvp.result", actual = rows_text(output)));
        }
        detail.push(t!("insight.nested_loops.tvp.limit"));
        draft.detail = detail.join(" ");
        draft.fix =
            statistics::relational_input_advice(crate::plan_facts::RelationalInputKind::TableValuedParameter, &input);
        draft.verify = t!("insight.nested_loops.tvp.verify");
        draft
    }
}

fn qualified_tvp_scan(node: &PlanNode) -> bool {
    node.kind == NodeKind::Scan
        && node.children.is_empty()
        && node.index.is_none()
        && node.seek_predicate.is_none()
        && node
            .facts()
            .relational_input
            .is_some_and(|input| input.qualified_kind() == crate::plan_facts::RelationalInputKind::TableValuedParameter)
        && node.facts().scan_completion.as_ref().is_some_and(|completion| completion.is_single_completed_serial_scan())
}

fn direct_inner_access(node: &PlanNode) -> bool {
    matches!(node.kind, NodeKind::Scan | NodeKind::Seek)
        && node.children.is_empty()
        && node.object.is_some()
        && !has_relational_input(node)
}

fn single_serial_path(cx: &Cx, join: &PlanNode, outer: &PlanNode) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    cx.plan.kind == PlanKind::Actual
        && cx.engine() == Engine::Mssql
        && join.facts().inner_join == Some(true)
        && serial_executions(join) == Some(1.0)
        && serial_executions(outer) == Some(1.0)
        && cx.nodes.iter().all(|node| ids.insert(node.id))
        && std::iter::once(join).chain(&join.children).all(|node| node.facts().estimated_rows_without_goal.is_none())
        && cx.ancestors(join).all(|parent| {
            matches!(parent.kind, NodeKind::Sort | NodeKind::Aggregate | NodeKind::Compute)
                && parent.facts().estimated_rows_without_goal.is_none()
                && serial_executions(parent) == Some(1.0)
        })
}

/// The source-issued count is exact native text. Binary64 totals are comparable only within its integer range,
/// and an embedder's contradictory worker/output facts cannot turn it into stronger serial evidence.
fn serial_executions(node: &PlanNode) -> Option<f64> {
    let native = node.facts().serial_execution_count.as_deref()?;
    let count = native.parse::<u64>().ok()?;
    (count > 0
        && count <= MAX_EXACT_COUNT
        && count.to_string() == native
        && node.act_executions == Some(count as f64)
        && node.facts().parallel_threads.is_none()
        && node.act_rows.is_some_and(exact_count))
    .then_some(count as f64)
}

fn exact_count(value: f64) -> bool {
    value.is_finite() && value >= 0.0 && value <= MAX_EXACT_COUNT as f64 && value.fract() == 0.0
}
