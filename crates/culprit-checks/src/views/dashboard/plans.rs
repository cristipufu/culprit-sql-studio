//! The dashboard's plans tile.

use super::*;

pub(super) fn plans(s: &Snapshot) -> Tile {
    let Some(w) = &s.workload else { return no_workload(TileKey::Plans, t!("view.dashboard.no_workload.plans")) };
    // Plans of queries that stopped running (most likely changed by a deploy) would describe code that is gone, and a
    // statement that ran once isn't what the application runs.
    let with_plans: Vec<&QueryStat> =
        app_queries(s).filter(|q| q.plan.is_some() && !crate::query_is_history(s, q)).collect();
    if with_plans.is_empty() {
        let total = app_queries(s).filter(|q| q.plan.is_some()).count();
        if total > 0 {
            let mut tile = Tile::new(
                TileKey::Plans,
                TileStatus::Info,
                t!("view.dashboard.plans.all_history", count = total.to_string()),
            );
            tile.metrics = vec![
                TileMetric::new("captured", &t!("view.dashboard.plans.metric.current"), Some(0.0), "0"),
                TileMetric::new("all", &t!("view.dashboard.metric.with_plans"), Some(total as f64), total.to_string()),
            ];
            tile.detail = Some(t!("view.dashboard.plans.history_detail"));
            return tile;
        }
        let reason = [Capability::QueryPlans, Capability::Showplan, Capability::GenericPlans]
            .into_iter()
            .find_map(|c| missing_reason(s, c))
            .map(|m| m.reason.clone());
        let mut t = Tile::new(
            TileKey::Plans,
            TileStatus::Unavailable,
            match reason {
                Some(reason) => t!("view.dashboard.plans.none_captured_because", reason = reason),
                None => t!("view.dashboard.plans.none_captured"),
            },
        );
        t.detail = Some(t!("view.dashboard.plans.none_detail"));
        return t;
    }
    struct Group<'a> {
        plans: usize,
        share: f64,
        severity: Severity,
        top: &'a QueryStat,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut flagged = 0;
    let mut observations = 0;
    for q in &with_plans {
        let Some(plan) = &q.plan else { continue };
        // What the plan-insights check reports: an insight capped at Low (a cheap plan, a small spill) isn't a fix.
        let reading = crate::query_plan_insights(s, q, plan);
        if !reading.items.is_empty() {
            observations += 1;
        }
        let found: Vec<_> = reading.items.into_iter().filter(|i| i.severity > Severity::Low).collect();
        if found.is_empty() {
            continue;
        }
        flagged += 1;
        // As every surface states it: over Culprit's scans where they measured it.
        let share = w.statement_share(q);
        let mut rules = HashSet::new();
        for i in &found {
            if !rules.insert(i.rule.clone()) {
                continue;
            }
            let g =
                groups.entry(i.rule.clone()).or_insert(Group { plans: 0, share: 0.0, severity: i.severity, top: q });
            g.plans += 1;
            g.share += share;
            g.severity = g.severity.max(i.severity);
            if q.total_time_ms > g.top.total_time_ms {
                g.top = q;
            }
        }
    }
    let captured = with_plans.len();
    let mut ranked: Vec<(String, Group)> = groups.into_iter().collect();
    ranked.sort_by(|a, b| b.1.severity.cmp(&a.1.severity).then(b.1.share.total_cmp(&a.1.share)).then(a.0.cmp(&b.0)));
    let (status, takeaway) = match ranked.first() {
        None if observations > 0 => (
            TileStatus::Info,
            t!("view.dashboard.plans.observations", n = observations, count = observations.to_string()),
        ),
        None => (TileStatus::Good, t!("view.dashboard.plans.clean", n = captured, count = captured.to_string())),
        Some((rule, g)) => {
            // Worth watching where the plan-insights check would raise it: a share of database time behind it.
            let status = if (g.severity == Severity::High && g.share >= crate::queries::MIN_SHARE)
                || (g.severity == Severity::Medium && g.share >= 0.1)
            {
                TileStatus::Watch
            } else {
                TileStatus::Info
            };
            let what = short_rule(rule, culprit_core::i18n::LabelPosition::Inline);
            let share = format::percent(g.share.min(1.0));
            let (flagged_count, captured_count) = (flagged.to_string(), captured.to_string());
            let takeaway = if flagged == 1 {
                t!("view.dashboard.plans.one_flagged", n = captured, count = captured_count, rule = what, share = share)
            } else if g.plans == 1 {
                t!(
                    "view.dashboard.plans.costliest",
                    flagged = flagged_count,
                    captured = captured_count,
                    rule = what,
                    share = share
                )
            } else {
                t!(
                    "view.dashboard.plans.shows_up",
                    n = g.plans,
                    flagged = flagged_count,
                    captured = captured_count,
                    rule = what,
                    count = g.plans.to_string(),
                    share = share
                )
            };
            (status, takeaway)
        }
    };
    let mut tile = Tile::new(TileKey::Plans, status, takeaway);
    let total = app_queries(s).filter(|q| q.plan.is_some()).count();
    let excluded = total - captured;
    let mut coverage = t!("view.dashboard.plans.coverage", n = captured, captured = captured.to_string(),);
    if excluded > 0 {
        coverage.push(' ');
        coverage.push_str(&t!("view.dashboard.plans.excluded", n = excluded, count = excluded.to_string()));
    }
    tile.detail = Some(coverage);
    tile.metrics = vec![
        TileMetric::new(
            "captured",
            &t!("view.dashboard.plans.metric.current"),
            Some(captured as f64),
            captured.to_string(),
        ),
        TileMetric::new(
            "flagged",
            &t!("view.dashboard.plans.metric.flagged"),
            Some(flagged as f64),
            flagged.to_string(),
        ),
    ];
    tile.items = ranked
        .iter()
        .take(4)
        .map(|(rule, g)| {
            let mut item = TileItem::new(
                ItemKind::Rule,
                rule.clone(),
                short_rule(rule, culprit_core::i18n::LabelPosition::Initial),
            );
            item.value = Some(t!("view.dashboard.plans.count", n = g.plans, count = g.plans.to_string()));
            item.share = Some(g.share.min(1.0));
            item.status = Some(severity_status(g.severity));
            item.query_id = Some(g.top.id.clone());
            item
        })
        .collect();
    tile
}

/// A rule as a few words for a sentence: "a large sort", "key lookups".
pub(super) fn short_rule(rule: &str, position: culprit_core::i18n::LabelPosition) -> String {
    match rule {
        "cross-table-or" => t!("view.dashboard.plans.rule.cross_table_or", position = position),
        "key-lookups" => t!("view.dashboard.plans.rule.key_lookups", position = position),
        "scan-filter" => t!("view.dashboard.plans.rule.scan_filter", position = position),
        "missing-index-hint" => t!("view.dashboard.plans.rule.missing_index_hint", position = position),
        "implicit-conversion" => t!("view.dashboard.plans.rule.implicit_conversion", position = position),
        "non-sargable" => t!("view.dashboard.plans.rule.non_sargable", position = position),
        "leading-wildcard" => t!("view.dashboard.plans.rule.leading_wildcard", position = position),
        "not-in" => t!("view.dashboard.plans.rule.not_in", position = position),
        "scalar-udf" => t!("view.dashboard.plans.rule.scalar_udf", position = position),
        "parallelism-blocked" => t!("view.dashboard.plans.rule.parallelism_blocked", position = position),
        "parallel-skew" => t!("view.dashboard.plans.rule.parallel_skew", position = position),
        "eager-spool" => t!("view.dashboard.plans.rule.eager_spool", position = position),
        "spill" => t!("view.dashboard.plans.rule.spill", position = position),
        "memory-grant" => t!("view.dashboard.plans.rule.memory_grant", position = position),
        "misestimate" => t!("view.dashboard.plans.rule.misestimate", position = position),
        "nested-loops" => t!("view.dashboard.plans.rule.nested_loops", position = position),
        "big-sort" => t!("view.dashboard.plans.rule.big_sort", position = position),
        "no-join-predicate" => t!("view.dashboard.plans.rule.no_join_predicate", position = position),
        "missing-statistics" => t!("view.dashboard.plans.rule.missing_statistics", position = position),
        "optimizer-timeout" => t!("view.dashboard.plans.rule.optimizer_timeout", position = position),
        "legacy-ce" => t!("view.dashboard.plans.rule.legacy_ce", position = position),
        "slow-compile" => t!("view.dashboard.plans.rule.slow_compile", position = position),
        "many-to-many" => t!("view.dashboard.plans.rule.many_to_many", position = position),
        "remote-query" => t!("view.dashboard.plans.rule.remote_query", position = position),
        "heap-fetches" => t!("view.dashboard.plans.rule.heap_fetches", position = position),
        "lossy-bitmap" => t!("view.dashboard.plans.rule.lossy_bitmap", position = position),
        "workers-short" => t!("view.dashboard.plans.rule.workers_short", position = position),
        "jit" => t!("view.dashboard.plans.rule.jit", position = position),
        "correlated-subquery" => t!("view.dashboard.plans.rule.correlated_subquery", position = position),
        "join-buffer" => t!("view.dashboard.plans.rule.join_buffer", position = position),
        "temp-table-sort" => t!("view.dashboard.plans.rule.temp_table_sort", position = position),
        "index-ignored" => t!("view.dashboard.plans.rule.index_ignored", position = position),
        // An external rule keeps its native identifier until it supplies a label.
        other => other.to_string(),
    }
}
