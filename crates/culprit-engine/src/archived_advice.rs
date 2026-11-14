//! Retained plan advice needs independently established current prerequisites before exposing executable changes.
use culprit_core::Plan;

pub(crate) fn withhold_plan_sql(plan: &mut Plan) -> bool {
    let mut changed = false;
    for insight in plan.insights.as_mut().into_iter().flat_map(|insights| &mut insights.items) {
        changed |= insight.fix_sql.take().is_some();
    }
    changed
}
