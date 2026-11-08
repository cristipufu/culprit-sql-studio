//! Plan insights about the server around the plan: settings, compiles and resources a plan reports.

use super::*;

pub(super) fn workers_short(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        // A Gather that never ran launched no workers because it never asked for any.
        .filter(|n| n.act_executions.is_none_or(|runs| runs > 0.0))
        .filter_map(|n| {
            let planned = n.facts().workers_planned.map(f64::from)?;
            let launched = n.facts().workers_launched.map(f64::from)?;
            (launched < planned).then_some((n, planned, launched))
        })
        .take(1)
        .map(|(n, planned, launched)| {
            let mut d = Draft::new("workers-short", InsightArea::Server, cx.subtree(n), vec![n.id]);
            d.subtree = true;
            d.cap = Severity::Medium;
            d.title = t!(
                "insight.workers_short.title",
                n = planned,
                planned = format::count(planned),
                launched = format::count(launched)
            );
            d.detail = t!("insight.workers_short.detail");
            d.fix = t!("insight.workers_short.fix");
            d.verify = t!("insight.workers_short.verify");
            d
        })
        .collect()
}

pub(super) fn heap_fetches(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        .filter(|n| n.op == "Index Only Scan")
        .filter_map(|n| {
            let fetches = n.facts().heap_fetches?;
            (fetches >= 1_000.0 && fetches >= 0.1 * rows_out(n)).then_some((n, fetches))
        })
        .take(1)
        .map(|(n, fetches)| {
            let table = what(n);
            let mut d = Draft::new("heap-fetches", InsightArea::Server, cx.share(n), vec![n.id]);
            d.cap = Severity::Medium;
            let leaf = table_leaf(&table);
            d.title = t!("insight.heap_fetches.title", table = &leaf, n = fetches, times = rows_text(fetches));
            d.detail = t!("insight.heap_fetches.detail");
            d.fix = t!("insight.heap_fetches.fix", table = &leaf);
            d.fix_sql = Some(format!("VACUUM (ANALYZE) {};", qualified(Engine::Postgres, &table)));
            d.verify = t!("insight.heap_fetches.verify");
            d
        })
        .collect()
}

pub(super) fn jit(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Postgres {
        return Vec::new();
    }
    let facts = cx.plan.facts();
    let Some(total) = facts.jit_time_ms else {
        return Vec::new();
    };
    let execution = facts.elapsed_ms;
    if total < 10.0 || execution.is_some_and(|e| total < 0.2 * e) {
        return Vec::new();
    }
    let share = execution.map(|e| (total / e.max(total)).min(1.0));
    // Execution Time includes the compiling. When compiling took at least as long as everything else, the compiled
    // code can't have won that back; below that it may have, and only a run without JIT can tell.
    let dominates = share.is_some_and(|s| s >= 0.5);
    let mut d = Draft::new("jit", InsightArea::Server, share.unwrap_or(0.2), vec![cx.plan.root.id]);
    d.fixed_weight = share.is_none();
    d.cost_ms = Some(total);
    d.cap = if dominates { Severity::Medium } else { Severity::Low };
    d.title = match execution {
        Some(e) => t!("insight.jit.title_of", time = format::duration(total), ran = format::duration(e)),
        None => t!("insight.jit.title", time = format::duration(total)),
    };
    d.detail = [
        t!("insight.jit.detail"),
        if dominates { t!("insight.jit.detail_dominates") } else { t!("insight.jit.detail_may_pay") },
    ]
    .join(" ");
    d.fix = if dominates { t!("insight.jit.fix_dominates") } else { t!("insight.jit.fix") };
    d.fix_sql = Some(if dominates {
        "-- In your session, then EXPLAIN ANALYZE again and compare:\nSET jit = off;\n-- If it's faster, for the application's role:\n-- ALTER ROLE app_user SET jit = off;".into()
    } else {
        "-- In your session only, then EXPLAIN ANALYZE again and compare:\nSET jit = off;".into()
    });
    d.verify = t!("insight.jit.verify");
    vec![d]
}
