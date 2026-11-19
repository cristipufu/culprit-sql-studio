//! Same native runtime inputs permit an observed operator-work comparison, never an exclusive parameter cause.
use super::*;
use crate::insights::{LookupReadComparison, lookup_read_comparison};

pub(super) fn scenario(pair: &NativeRuntimeInputPair<'_>, name_a: &str, name_b: &str) -> Option<Scenario> {
    if pair.relation != NativeRuntimeInputRelation::Same
        || pair.a.engine != Engine::Mssql
        || pair.b.engine != pair.a.engine
        || pair.a.kind != PlanKind::Actual
        || pair.b.kind != PlanKind::Actual
        || pair.a.statement.as_deref().is_none_or(|text| text.is_empty() || Some(text) != pair.b.statement.as_deref())
    {
        return None;
    }
    let (comparison, reused_name, scan_name, reversed) = match lookup_read_comparison(pair.a, pair.b) {
        Some(comparison) => (comparison, name_a, name_b, false),
        None => (lookup_read_comparison(pair.b, pair.a)?, name_b, name_a, true),
    };
    let LookupReadComparison { reused_node, scan_node, executions, lookup_reads, scan_reads } = comparison;
    Some(Scenario {
        kind: ScenarioKind::Parameters,
        title: t!("core.plan_compare.scalar_reuse.title", reused = reused_name, scan = scan_name),
        detail: Some(format!(
            "{} {}",
            t!(
                "core.plan_compare.scalar_reuse.detail",
                reused = reused_name,
                scan = scan_name,
                executions = format::count(executions),
                lookup_reads = format::count(lookup_reads),
                scan_reads = format::count(scan_reads)
            ),
            t!("core.plan_compare.scalar_reuse.limit")
        )),
        a: Some(if reversed { scan_node } else { reused_node }),
        b: Some(if reversed { reused_node } else { scan_node }),
    })
}
