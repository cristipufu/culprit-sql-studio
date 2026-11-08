//! Plan insights about memory: sorts and hashes that spill, and grants far larger or smaller than the run used.

use super::*;

pub(super) fn memory_grant(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Mssql {
        return Vec::new();
    }
    let facts = cx.plan.facts();
    let mut out = Vec::new();
    let root = cx.plan.root.id;
    if let (Some(granted), Some(used)) = (facts.memory_granted_kb, facts.memory_max_used_kb)
        && has_actuals(cx.plan)
        && granted >= 10.0 * 1024.0
        && used < 0.25 * granted
    {
        let mut d = Draft::new("memory-grant", InsightArea::Memory, 0.15, vec![memory_node(cx).unwrap_or(root)]);
        d.fixed_weight = true;
        d.subtree = true;
        d.floor = if granted >= 1024.0 * 1024.0 { Severity::High } else { Severity::Medium };
        d.title = t!(
            "insight.memory_grant.oversized.title",
            granted = format::bytes(granted * 1024.0),
            used = format::bytes(used * 1024.0)
        );
        let mut detail = vec![t!("insight.memory_grant.oversized.detail")];
        detail.extend(
            facts.memory_grant_warning.as_ref().map(|w| t!("insight.memory_grant.oversized.warned", warning = w)),
        );
        d.detail = detail.join(" ");
        d.fix = t!("insight.memory_grant.oversized.fix");
        d.verify = t!("insight.memory_grant.oversized.verify");
        out.push(d);
    } else if let Some(desired) = facts.memory_desired_kb
        && !has_actuals(cx.plan)
        && desired >= 1024.0 * 1024.0
    {
        let mut d = Draft::new("memory-grant", InsightArea::Memory, 0.1, vec![memory_node(cx).unwrap_or(root)]);
        d.fixed_weight = true;
        // A gigabyte or more asked for each run is enough on its own to make others queue.
        d.floor = Severity::Medium;
        d.subtree = true;
        d.title = t!("insight.memory_grant.desired.title", size = format::bytes(desired * 1024.0));
        d.detail = t!("insight.memory_grant.desired.detail");
        d.fix = t!("insight.memory_grant.desired.fix");
        d.verify = t!("insight.memory_grant.desired.verify");
        out.push(d);
    }
    if let Some(wait) = facts.memory_grant_wait_ms.filter(|w| *w >= 100.0) {
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
