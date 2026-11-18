//! Link one original, completely read TVP to a measured oversized grant without claiming that it sized the grant.
use super::*;

pub(in crate::insights) struct GrantInput<'a> {
    pub(in crate::insights) node: &'a PlanNode,
    comparison: AccessComparison,
}

impl GrantInput<'_> {
    pub(in crate::insights) fn detail(&self) -> String {
        t!(
            "insight.memory_grant.tvp.input",
            input = what(self.node),
            estimated = rows_text(self.comparison.estimated),
            actual = rows_text(self.comparison.actual)
        )
    }
}

/// The native completion and path checks are shared with input-cardinality explanations. A TOP, seek, repeated
/// or parallel access cannot turn rows touched into the whole TVP. A separate memory-consuming branch cannot
/// establish a grant relationship to this input; even the same branch remains a candidate, not a sizing proof.
pub(in crate::insights) fn oversized_grant_input<'a>(cx: &Cx<'a>) -> Option<GrantInput<'a>> {
    let memory = memory::memory_node(cx)?;
    cx.nodes
        .iter()
        .filter_map(|node| {
            let comparison = AccessComparison::from_node(cx, node)?;
            (comparison.overestimated_complete_input() && cx.ancestors(node).any(|parent| parent.id == memory))
                .then_some(GrantInput { node, comparison })
        })
        .max_by(|left, right| left.comparison.estimated.total_cmp(&right.comparison.estimated))
}
