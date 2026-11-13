//! The dashboard's waits tile.

use super::*;

/// Waiting of `ms` is a real amount over the stretch the report covers: about a minute a day, over at least an hour of
/// it. A sample taken just now needs a second of waiting in it.
fn material(s: &Snapshot, scope: WaitScope, ms: f64) -> bool {
    let hours = match scope {
        WaitScope::Window | WaitScope::Statements => s.workload.as_ref().map(|w| w.measurement_hours()),
        WaitScope::SinceStartup => s.pressure.as_ref().and_then(|p| p.uptime_hours),
        WaitScope::Sampled => return ms >= 1_000.0,
    };
    hours.filter(|h| *h >= culprit_core::RATE_MIN_HOURS).is_some_and(|h| ms / h * 24.0 >= 60_000.0)
}

pub(super) fn waits(s: &Snapshot) -> Tile {
    let report = crate::waits::explain(s);
    if let Some(activity) = report.as_ref().and_then(|r| r.activity.as_ref()) {
        let p = &activity.period;
        let takeaway = activity.categories.first().map_or_else(
            || t!("view.dashboard.waits.activity_none"),
            |c| t!("view.dashboard.waits.activity_share", category = &c.label, share = format::percent(c.share)),
        );
        let mut tile = Tile::new(TileKey::Waits, TileStatus::Info, takeaway);
        tile.title = t!("view.dashboard.waits.activity_title");
        let sample = |at: Option<&str>| at.map_or_else(|| t!("view.dashboard.unavailable"), str::to_string);
        let mut detail = vec![t!(
            "view.dashboard.waits.activity_period",
            source = &p.source,
            first = sample(p.first_sample.as_deref()),
            last = sample(p.last_sample.as_deref())
        )];
        if p.partial {
            detail.push(t!("view.dashboard.waits.partial"));
        }
        detail.push(t!("view.dashboard.waits.activity_scope"));
        tile.detail = Some(detail.join(" "));
        tile.metrics = vec![
            TileMetric::new(
                "observations",
                &t!("view.dashboard.waits.metric.observations"),
                Some(p.observations),
                format::count(p.observations),
            ),
            TileMetric::new(
                "sample-weight",
                &t!("view.dashboard.waits.metric.sample_weight"),
                Some(p.weighted_observations),
                format::count(p.weighted_observations),
            ),
        ];
        tile.items = activity
            .categories
            .iter()
            .take(6)
            .map(|c| {
                let mut item = TileItem::new(ItemKind::Wait, c.key.clone(), c.label.clone());
                item.share = Some(c.share);
                item.value = Some(t!("view.dashboard.waits.of_sample_weight", share = format::percent(c.share)));
                item
            })
            .collect();
        return tile;
    }
    let report = report.filter(|r| !r.categories.is_empty());
    let Some(r) = report else { return waits_missing(s) };
    let top = &r.categories[0];
    let named = r.categories.iter().find(|c| c.key != "other");
    // The slow log's native lock timer is a recorded total, not a measured portion of query elapsed time.
    if r.scope == WaitScope::Statements
        && s.workload.as_ref().and_then(|workload| workload.kind()) == Some(culprit_core::WorkloadKind::SlowLog)
    {
        let total_ms = r.total_ms.unwrap_or(0.0);
        let takeaway = t!(
            "view.dashboard.waits.mysql_lock_time",
            family = family(&s.server),
            duration = format::duration(total_ms)
        );
        let mut tile = Tile::new(TileKey::Waits, TileStatus::Info, takeaway);
        tile.detail = top.detail.clone();
        tile.metrics = vec![TileMetric::new(
            "total",
            &t!("view.dashboard.waits.metric.lock_time"),
            r.total_ms,
            format::duration(total_ms),
        )];
        tile.items = r
            .categories
            .iter()
            .take(6)
            .map(|c| {
                let mut item = TileItem::new(ItemKind::Wait, c.key.clone(), c.label.clone());
                item.value = Some(format::duration(c.wait_ms));
                item.query_id = c.queries.first().map(|q| q.id.clone());
                item
            })
            .collect();
        return tile;
    }
    let takeaway = if top.key == "other" {
        // "Everything else" leading says nothing; say what is known and how much isn't.
        match named {
            Some(c) => t!(
                "view.dashboard.waits.partly_named",
                category = &c.label,
                share = format::percent(c.share),
                other = format::percent(top.share),
                family = family(&s.server)
            ),
            None => t!("view.dashboard.waits.unnamed", family = family(&s.server)),
        }
    } else {
        match r.categories.get(1).filter(|c| c.share >= 0.05) {
            Some(next) => t!(
                "view.dashboard.waits.most_then",
                top = in_sentence(&top.key),
                top_share = format::percent(top.share),
                next = in_sentence(&next.key),
                next_share = format::percent(next.share)
            ),
            None => {
                t!("view.dashboard.waits.most", top = in_sentence(&top.key), top_share = format::percent(top.share))
            }
        }
    };
    let contended: f64 =
        r.categories.iter().filter(|c| matches!(c.key.as_str(), "lock" | "latch" | "memory")).map(|c| c.share).sum();
    // Contention worth watching is a real amount of it, and this database's: totals since startup are the whole
    // server's (every database on it), except on Azure SQL Database where the server is the database.
    let shared = r.scope == WaitScope::SinceStartup
        && !s.server.platform().is_some_and(|p| p.is_azure_sql_database())
        && s.server.engine.has_server();
    let watch = contended >= 0.25 && !shared && r.total_ms.is_some_and(|ms| material(s, r.scope, contended * ms));
    let status = if watch { TileStatus::Watch } else { TileStatus::Info };
    let mut tile = Tile::new(TileKey::Waits, status, takeaway);
    let lead = if top.key == "other" { named.unwrap_or(top) } else { top };
    // Totals since startup cover a different stretch than database time: say which, rather than set them side by side.
    let since_start = r.scope == WaitScope::SinceStartup;
    tile.detail = if since_start {
        Some(format!("{}.", r.source.trim_end_matches('.')))
    } else {
        lead.detail.clone().or_else(|| Some(culprit_core::waits::category_gist(&lead.key, s.server.engine)))
    };
    let label = if since_start {
        t!("view.dashboard.waits.metric.since_start")
    } else {
        t!("view.dashboard.waits.metric.waiting")
    };
    tile.metrics = vec![TileMetric::new(
        "total",
        &label,
        r.total_ms,
        r.total_ms.map(format::duration).unwrap_or_else(|| t!("view.dashboard.unavailable")),
    )];
    tile.items = r
        .categories
        .iter()
        .take(6)
        .map(|c| {
            let mut item = TileItem::new(ItemKind::Wait, c.key.clone(), c.label.clone());
            item.value = Some(format::percent(c.share));
            item.share = Some(c.share);
            item.query_id = c.queries.first().map(|q| q.id.clone());
            item
        })
        .collect();
    tile
}

/// A kind of waiting (its key, `culprit_core::waits::category_key`) as a sentence names it after its first word: "Most
/// time goes to disk reads". Its own message, never its label lowercased: a language that capitalizes nouns keeps them.
pub(super) fn in_sentence(key: &str) -> String {
    match key {
        "rpc" => t!("view.dashboard.waits.category.rpc"),
        "storage" => t!("view.dashboard.waits.category.storage"),
        "cpu" => t!("view.dashboard.waits.category.cpu"),
        "workers" => t!("view.dashboard.waits.category.workers"),
        "io" => t!("view.dashboard.waits.category.io"),
        "rows" => t!("view.dashboard.waits.category.rows"),
        "lock" => t!("view.dashboard.waits.category.lock"),
        "latch" => t!("view.dashboard.waits.category.latch"),
        "log" => t!("view.dashboard.waits.category.log"),
        "network" => t!("view.dashboard.waits.category.network"),
        "parallelism" => t!("view.dashboard.waits.category.parallelism"),
        "memory" => t!("view.dashboard.waits.category.memory"),
        "compilation" => t!("view.dashboard.waits.category.compilation"),
        _ => t!("view.dashboard.waits.category.other"),
    }
}

pub(super) fn waits_missing(s: &Snapshot) -> Tile {
    let family = family(&s.server);
    let mut tile = match s.server.engine {
        Engine::Yugabyte => Tile::new(TileKey::Waits, TileStatus::Unavailable, t!("view.dashboard.waits.yugabyte")),
        Engine::Sqlite => Tile::new(TileKey::Waits, TileStatus::Unavailable, t!("view.dashboard.waits.sqlite")),
        Engine::Postgres => {
            let mut tile = Tile::new(TileKey::Waits, TileStatus::Unavailable, t!("view.dashboard.waits.postgres"));
            if is_off(setting(s, "track_io_timing")) {
                // Where it is set: ALTER SYSTEM on a server of one's own, the platform's settings on a managed one.
                let change = crate::support::setting_change(&s.server, "track_io_timing", "on");
                tile.detail = Some([t!("view.dashboard.waits.postgres_io_timing"), change.how].join(" "));
                tile.fix_sql = change.sql;
            } else {
                tile.detail = Some(t!("view.dashboard.waits.postgres_sample"));
            }
            tile
        }
        Engine::Mysql => match missing_reason(s, Capability::PerformanceSchema)
            .or_else(|| missing_reason(s, Capability::WaitStats))
        {
            Some(m) => {
                let mut tile = Tile::new(
                    TileKey::Waits,
                    TileStatus::Unavailable,
                    t!("view.dashboard.waits.not_counting", family = family, reason = m.reason.clone()),
                );
                tile.detail = m.fix.clone();
                tile
            }
            None => Tile::new(
                TileKey::Waits,
                TileStatus::Unavailable,
                t!("view.dashboard.waits.none_recorded", family = family),
            ),
        },
        Engine::Mssql => match missing_reason(s, Capability::WaitStats) {
            Some(m) => {
                let mut tile = Tile::new(
                    TileKey::Waits,
                    TileStatus::Unavailable,
                    t!("view.dashboard.waits.unreadable", reason = m.reason.clone()),
                );
                tile.detail = m.fix.clone();
                tile
            }
            None => Tile::new(TileKey::Waits, TileStatus::Unavailable, t!("view.dashboard.waits.none")),
        },
    };
    if s.server.engine == Engine::Yugabyte {
        tile.detail = s.source_problem(&[culprit_core::source_id::DOCDB_ACTIVITY_HISTORY]);
    }
    if tile.detail.is_none() && s.server.engine.has_server() {
        tile.detail = Some(t!("view.dashboard.waits.sample_instead"));
    }
    tile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_log_native_lock_totals_do_not_become_elapsed_shares_or_pressure_grades() {
        for product in ["MySQL 8.0.28", "MariaDB 11.8.9"] {
            let mut snapshot = crate::waits::snapshot_with(Engine::Mysql);
            snapshot.server.product = product.into();
            snapshot.server.capabilities.retain(|capability| *capability != Capability::PerformanceSchema);
            let workload = snapshot.workload.as_mut().unwrap();
            workload.kind = Some(culprit_core::WorkloadKind::SlowLog);
            workload.total_time_ms = 1_000.0;
            snapshot.waits = vec![culprit_core::WaitStat {
                category: "Lock".into(),
                wait_type: Some("native lock time".into()),
                wait_ms: 6_000.0,
                tasks: None,
            }];
            let tile = waits(&snapshot);
            assert_eq!(tile.status, TileStatus::Info);
            assert_eq!(tile.metrics[0].value, Some(6_000.0));
            assert!(tile.takeaway.contains("6.0 s of native lock time"), "{}", tile.takeaway);
            assert!(!tile.takeaway.contains('%') && !tile.takeaway.contains("of their time"), "{}", tile.takeaway);
        }
    }

    #[test]
    fn native_category_totals_never_become_statement_elapsed_or_cpu_time() {
        for engine in Engine::ALL {
            let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
            snapshot.server.engine = engine;
            snapshot.waits =
                vec![culprit_core::WaitStat { category: "IO".into(), wait_type: None, wait_ms: 6_000.0, tasks: None }];
            let tile = waits(&snapshot);
            assert_ne!(tile.metrics[0].label, "Statement execution time", "{engine:?}");
            assert!(
                !tile.detail.as_deref().is_some_and(|detail| detail.contains("including CPU and unclassified time")),
                "{engine:?}"
            );
        }
    }
}
