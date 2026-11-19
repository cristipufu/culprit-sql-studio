//! Observed repeated lookup work under one cached parameterized access. This does not prove harmful reuse.
use super::*;

const MIN_LOOKUP_LOGICAL_READS: f64 = MANY_RUNS;

pub(in crate::insights) struct LookupWork<'a> {
    parameter: &'a PlanParameter,
    join: &'a PlanNode,
    lookup: &'a PlanNode,
    estimated: f64,
    actual: f64,
    executions: f64,
    logical_reads: f64,
}

impl<'a> LookupWork<'a> {
    pub(in crate::insights) fn from_plan(cx: &Cx<'a>, lookup: &'a PlanNode) -> Option<Self> {
        let context = cx.plan.facts().parameter_context.as_ref()?;
        if context.retrieved_from_cache != Some(true) {
            return None;
        }
        let join = cx.parent(lookup)?;
        let [seek, inner] = join.children.as_slice() else { return None };
        if inner.id != lookup.id || !qualified_pair(cx, join, seek, lookup) {
            return None;
        }
        let access = access_mismatch(cx, seek)?;
        if access.parameter.integer_values_equal != Some(false) {
            return None;
        }
        let (estimated, actual) = access.comparison.output_counts()?;
        let executions = serial_executions(lookup)?;
        let logical_reads = lookup.runtime.as_ref()?.logical_reads.filter(|reads| exact_count(*reads))?;
        if actual < MANY_RUNS
            || actual < MISESTIMATE_FACTOR * estimated.max(1.0)
            || access.comparison.actual_accessed() != actual
            || executions != actual
            || lookup.act_rows != Some(actual)
            || join.act_rows != Some(actual)
            || logical_reads < MIN_LOOKUP_LOGICAL_READS
        {
            return None;
        }
        Some(Self { parameter: access.parameter, join, lookup, estimated, actual, executions, logical_reads })
    }

    pub(in crate::insights) fn enrich(self, draft: &mut Draft) {
        draft.nodes.push(self.join.id);
        draft.detail = format!(
            "{} {} {}",
            draft.detail,
            t!(
                "insight.parameter_lookup.detail",
                parameter = parameter_label(self.parameter),
                estimated = rows_text(self.estimated),
                actual = rows_text(self.actual),
                node = format::digits(self.lookup.id.to_string()),
                executions = format::count(self.executions),
                reads = format::count(self.logical_reads)
            ),
            t!("insight.parameter_lookup.limit")
        );
        draft.fix = t!("insight.parameter_lookup.fix", covering = &draft.fix);
        draft.verify = t!("insight.parameter_lookup.verify");
    }
}

fn qualified_pair(cx: &Cx<'_>, join: &PlanNode, seek: &PlanNode, lookup: &PlanNode) -> bool {
    join.kind == NodeKind::Join
        && join.op == "Nested Loops"
        && join.facts().inner_join == Some(true)
        && serial_executions(join) == Some(1.0)
        && serial_executions(seek) == Some(1.0)
        && lookup.kind == NodeKind::Lookup
        && lookup.children.is_empty()
        && seek.children.is_empty()
        && lookup.index.is_some()
        && seek.index.is_some()
        && seek.predicate.is_none()
        && lookup.predicate.is_none()
        && lookup.seek_predicate.is_some()
        && same_native_relation(cx, seek, lookup)
        && [join, seek, lookup].into_iter().all(|node| {
            node.facts().parallel_operator == Some(false) && node.facts().estimated_rows_without_goal.is_none()
        })
        && cx.ancestors(join).all(|node| {
            matches!(node.kind, NodeKind::Aggregate | NodeKind::Sort | NodeKind::Compute)
                && node.facts().parallel_operator == Some(false)
                && node.facts().estimated_rows_without_goal.is_none()
                && serial_executions(node) == Some(1.0)
        })
}

fn same_native_relation(cx: &Cx<'_>, seek: &PlanNode, lookup: &PlanNode) -> bool {
    let (Some(object), Some(alias)) = (seek.object.as_ref(), seek.facts().table_alias.as_ref()) else { return false };
    lookup.object.as_ref() == Some(object)
        && lookup.facts().table_alias.as_ref() == Some(alias)
        && !cx.nodes.iter().any(|node| {
            node.id != seek.id
                && node.id != lookup.id
                && node.object.is_some()
                && node.facts().table_alias.as_ref() == Some(alias)
        })
}
