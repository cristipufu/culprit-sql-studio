//! Structural scaling observations. They preserve pagination/partition semantics and never infer an index from shape.
use super::*;

pub(super) fn deep_pagination(cx: &Cx) -> Vec<Draft> {
    let Some(offset) =
        cx.plan.statement.as_deref().and_then(|s| crate::sql::offset_count(s, cx.engine())).filter(|v| *v >= 10_000.0)
    else {
        return vec![];
    };
    let n = cx
        .nodes
        .iter()
        .copied()
        .find(|n| n.op.to_ascii_lowercase().starts_with("limit") || n.op == "Top")
        .unwrap_or(&cx.plan.root);
    let mut d = Draft::new("deep-pagination", InsightArea::Query, cx.subtree(n), vec![n.id]);
    d.subtree = true;
    d.cap = Severity::Medium;
    d.title = t!("insight.deep_pagination.title", n = offset, count = format::count(offset));
    d.detail = t!("insight.deep_pagination.detail");
    d.fix = t!("insight.deep_pagination.fix");
    d.verify = t!("insight.deep_pagination.verify");
    vec![d]
}

pub(super) fn partition_filtering(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Postgres || cx.plan.kind != PlanKind::Actual {
        return vec![];
    }
    let mut groups: std::collections::BTreeMap<String, Vec<&PlanNode>> = Default::default();
    for n in &cx.nodes {
        if n.act_executions.is_some_and(|v| v > 0.0)
            && n.predicate.is_some()
            && n.act_rows.is_some()
            && let Some(parent) = n.facts().partition_parent.as_ref()
        {
            groups.entry(parent.clone()).or_default().push(n);
        }
    }
    groups
        .into_iter()
        .filter_map(|(parent, nodes)| {
            let unique: std::collections::HashSet<_> = nodes.iter().filter_map(|n| n.object.as_deref()).collect();
            let read: f64 = nodes.iter().map(|n| rows_read(n)).sum();
            let kept: f64 = nodes.iter().map(|n| rows_out(n)).sum();
            if unique.len() < 16 || read < 100_000.0 || kept / read.max(1.0) > INDEX_KEEPS_AT_MOST {
                return None;
            }
            let mut d =
                Draft::new("partition-filtering", InsightArea::Query, 0.1, nodes.iter().map(|n| n.id).collect());
            d.fixed_weight = true;
            d.cap = Severity::Low;
            d.title = t!(
                "insight.partition_filtering.title",
                n = unique.len(),
                count = format::digits(unique.len().to_string()),
                table = &parent
            );
            d.detail = t!(
                "insight.partition_filtering.detail",
                n = read,
                read = format::count(read),
                returned = format::count(kept)
            );
            d.fix = t!("insight.partition_filtering.fix");
            d.verify = t!("insight.partition_filtering.verify");
            Some(d)
        })
        .collect()
}
