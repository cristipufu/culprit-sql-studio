//! Plan insights about memory: sorts and hashes that spill, and grants far larger or smaller than the run used.

use super::*;

const MIN_OVERSIZED_GRANT_KIB: f64 = 10.0 * 1024.0;
const MAX_USED_GRANT_FRACTION: f64 = 0.25;
const LARGE_GRANT_KIB: f64 = 1024.0 * 1024.0;
const MIN_GRANT_WAIT_MS: f64 = 100.0;

pub(super) fn memory_grant_feedback(cx: &Cx) -> Vec<Draft> {
    let Some(feedback) = crate::plan_facts::PlanMemoryGrantFeedback::disabled_in(cx.plan) else {
        return Vec::new();
    };
    let mut concern = Draft::new(MEMORY_GRANT_FEEDBACK_RULE, InsightArea::Memory, 0.0, vec![cx.plan.root.id]);
    concern.fixed_weight = true;
    concern.cap = Severity::Low;
    concern.title = t!("insight.memory_grant_feedback.title");
    let mut detail = vec![t!("insight.memory_grant_feedback.detail")];
    detail.extend(feedback.previous_grant_kb.as_ref().map(|kilobytes| {
        t!("insight.memory_grant_feedback.previous_grant", kilobytes = format::digits(kilobytes.clone()))
    }));
    detail.push(t!("insight.memory_grant_feedback.limit"));
    concern.detail = detail.join(" ");
    concern.fix = t!("insight.memory_grant_feedback.fix");
    concern.verify = t!("insight.memory_grant_feedback.verify");
    vec![concern]
}

pub(super) fn memory_grant(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Mssql {
        return Vec::new();
    }
    let facts = cx.plan.facts();
    let mut out = Vec::new();
    let root = cx.plan.root.id;
    let measured_memory = |value: f64| value.is_finite() && value >= 0.0;
    if let (Some(granted), Some(used)) = (
        facts.memory_granted_kb.filter(|value| measured_memory(*value)),
        facts.memory_max_used_kb.filter(|value| measured_memory(*value)),
    ) && has_actuals(cx.plan)
        && granted >= MIN_OVERSIZED_GRANT_KIB
        && used < MAX_USED_GRANT_FRACTION * granted
    {
        out.push(oversized_grant(cx, granted, used));
    } else if let Some(desired) = facts.memory_desired_kb.filter(|value| measured_memory(*value))
        && !has_actuals(cx.plan)
        && desired >= LARGE_GRANT_KIB
    {
        let mut d = Draft::new("memory-grant", InsightArea::Memory, 0.1, vec![memory_node(cx).unwrap_or(root)]);
        d.fixed_weight = true;
        // A large compiled desire is plan context; actual reservation and pressure remain unproved.
        d.cap = Severity::Low;
        d.subtree = true;
        d.title = t!("insight.memory_grant.desired.title", size = format::bytes(desired * 1024.0));
        d.detail = t!("insight.memory_grant.desired.detail");
        d.fix = t!("insight.memory_grant.desired.fix");
        d.verify = t!("insight.memory_grant.desired.verify");
        out.push(d);
    }
    if let Some(wait) = facts.memory_grant_wait_ms.filter(|w| w.is_finite() && *w >= MIN_GRANT_WAIT_MS) {
        let mut d = Draft::new("memory-grant", InsightArea::Memory, 0.2, vec![root]);
        d.fixed_weight = true;
        d.cost_ms = Some(wait);
        d.floor = Severity::Medium;
        d.title = t!("insight.memory_grant.wait.title", time = format::duration(wait));
        d.detail = t!("insight.memory_grant.wait.detail");
        d.fix = t!("insight.memory_grant.wait.fix");
        d.verify = t!("insight.memory_grant.wait.verify");
        out.push(d);
    }
    out
}

fn oversized_grant(cx: &Cx, granted: f64, used: f64) -> Draft {
    let mut draft =
        Draft::new("memory-grant", InsightArea::Memory, 0.15, vec![memory_node(cx).unwrap_or(cx.plan.root.id)]);
    draft.fixed_weight = true;
    draft.subtree = true;
    draft.floor = if granted >= LARGE_GRANT_KIB { Severity::High } else { Severity::Medium };
    draft.title = t!(
        "insight.memory_grant.oversized.title",
        granted = format::bytes(granted * 1024.0),
        used = format::bytes(used * 1024.0)
    );
    let mut detail = vec![t!("insight.memory_grant.oversized.detail")];
    detail.extend(
        cx.plan
            .facts()
            .memory_grant_warning
            .as_ref()
            .map(|warning| t!("insight.memory_grant.oversized.warned", warning = warning)),
    );
    draft.fix = t!("insight.memory_grant.oversized.fix");
    draft.verify = t!("insight.memory_grant.oversized.verify");
    if let Some(input) = relational_inputs::oversized_grant_input(cx) {
        detail.push(input.detail());
        if !draft.nodes.contains(&input.node.id) {
            draft.nodes.push(input.node.id);
        }
        draft.fix = t!("insight.memory_grant.tvp.investigate");
        draft.verify = t!("insight.memory_grant.tvp.verify");
    }
    draft.detail = detail.join(" ");
    draft
}

pub(super) fn memory_node(cx: &Cx) -> Option<u32> {
    cx.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Sort || n.op.contains("Hash"))
        .max_by(|a, b| a.self_cost.total_cmp(&b.self_cost))
        .map(|n| n.id)
}

pub(super) fn lossy_bitmap(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        .filter_map(|n| {
            let lossy = n.facts().lossy_heap_blocks?;
            // A BRIN index only ever gives lossy pages: no exact ones means nothing was lost for want of work_mem.
            let exact = n.facts().exact_heap_blocks.unwrap_or(0.0);
            (lossy > 0.0 && exact > 0.0).then_some((n, lossy))
        })
        .take(1)
        .map(|(n, lossy)| {
            // PostgreSQL gives it per loop, like its actual rows.
            let rechecked = n.facts().rows_removed_by_recheck.unwrap_or(0.0) * n.act_executions.unwrap_or(1.0).max(1.0);
            // A BRIN index only ever gives pages: one in the bitmap (the catalog marks its kind) explains lossy pages
            // with work_mem to spare. Bitmaps combined (BitmapOr, BitmapAnd) on a plan the catalog didn't mark may hold
            // one, so work_mem is then one cause, not the cause.
            fn walk<'a>(n: &'a PlanNode, out: &mut Vec<&'a PlanNode>) {
                out.push(n);
                n.children.iter().for_each(|c| walk(c, out));
            }
            let mut below = Vec::new();
            walk(n, &mut below);
            let index_scans: Vec<&&PlanNode> = below.iter().filter(|x| x.op == "Bitmap Index Scan").collect();
            let marked = !index_scans.is_empty() && index_scans.iter().all(|x| x.facts().index_method.is_some());
            let combined = below.iter().any(|x| matches!(x.op.as_str(), "BitmapOr" | "BitmapAnd"));
            let maybe_brin = if marked { index_scans.iter().any(|x| x.reads_brin()) } else { combined };
            let mut d = Draft::new("lossy-bitmap", InsightArea::Memory, cx.share(n), vec![n.id]);
            d.cap = if maybe_brin { Severity::Low } else { Severity::Medium };
            let (table, pages) = (table_leaf(&what(n)), format::count(lossy));
            d.title = if maybe_brin {
                t!("insight.lossy_bitmap.title_pages", table = &table, n = lossy, pages = &pages)
            } else {
                t!("insight.lossy_bitmap.title", table = &table)
            };
            let mut detail = vec![if rechecked > 0.0 {
                t!(
                    "insight.lossy_bitmap.detail_rechecked",
                    n = lossy,
                    pages = &pages,
                    rechecked = format::count(rechecked)
                )
            } else {
                t!("insight.lossy_bitmap.detail", n = lossy, pages = &pages)
            }];
            if maybe_brin {
                detail.push(t!("insight.lossy_bitmap.detail_brin"));
            }
            d.detail = detail.join(" ");
            d.fix = t!("insight.lossy_bitmap.fix");
            d.fix_sql = Some("SET work_mem = '32MB';  -- test in your session first".into());
            d.verify = t!("insight.lossy_bitmap.verify");
            d
        })
        .collect()
}
