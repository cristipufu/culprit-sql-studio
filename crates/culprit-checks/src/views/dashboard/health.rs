//! The dashboard's health tile.

use super::*;

pub(super) fn health(report: &Report) -> Tile {
    let s = &report.snapshot;
    if s.pressure.as_ref().is_none_or(|p| p.signals.is_empty()) {
        if let Some(resources) = &s.operations.node_resources {
            let available = resources.nodes.iter().filter(|n| n.available).count();
            let mut tile = Tile::new(
                TileKey::Health,
                if available > 0 { TileStatus::Info } else { TileStatus::Unavailable },
                if available > 0 {
                    t!("view.dashboard.health.nodes", n = available, count = available.to_string())
                } else {
                    t!("view.dashboard.health.no_nodes")
                },
            );
            let mut detail = vec![t!(
                "view.dashboard.health.nodes_observed",
                source = &resources.source,
                from = &resources.observed_from,
                until = &resources.observed_until
            )];
            if resources.partial {
                detail.push(t!("view.dashboard.partial_read"));
            }
            detail.push(t!("view.dashboard.health.nodes_scope"));
            tile.detail = Some(detail.join(" "));
            tile.metrics.push(TileMetric::new(
                "available-nodes",
                &t!("view.dashboard.health.metric.nodes_available"),
                Some(available as f64),
                available.to_string(),
            ));
            tile.metrics.push(TileMetric::new(
                "expected-nodes",
                &t!("view.dashboard.health.metric.nodes_expected"),
                resources.nodes_expected.map(|n| n as f64),
                resources.nodes_expected.map_or_else(|| t!("view.dashboard.unknown"), |n| n.to_string()),
            ));
            return operational(report, tile);
        }
        if let Some(reason) = s.source_problem(&[culprit_core::source_id::DOCDB_NODE_RESOURCES]) {
            let mut tile =
                Tile::new(TileKey::Health, TileStatus::Unavailable, t!("view.dashboard.health.nodes_unavailable"));
            tile.detail = Some(reason);
            return operational(report, tile);
        }
    }
    let Some(p) = &s.pressure else {
        let o = &s.operations;
        if !o.spaces.is_empty()
            || !o.counters.is_empty()
            || !o.sequences.is_empty()
            || !o.multixacts.is_empty()
            || o.sqlite.is_some()
            || o.cluster.is_some()
        {
            return operational(
                report,
                Tile::new(TileKey::Health, TileStatus::Info, t!("view.dashboard.health.operational")),
            );
        }
        let engine = s.server.engine.to_string();
        let headline = if s.server.engine.has_server() {
            t!("view.dashboard.health.none")
        } else {
            t!("view.dashboard.health.serverless", engine = &engine)
        };
        let mut tile = Tile::new(TileKey::Health, TileStatus::Unavailable, headline);
        tile.detail = Some(match s.notes.iter().find(|n| n.collector.contains("pressure")) {
            Some(note) => note.message.clone(),
            None if !s.server.engine.has_server() => t!("view.dashboard.health.serverless_detail", engine = &engine),
            None => t!("view.dashboard.health.scan_again"),
        });
        return tile;
    };
    let mut signals: Vec<&Signal> = p.signals.iter().collect();
    // Worst first, then the healthy ones, then the ones shown without a verdict: an ungraded counter must never push a
    // healthy signal out of the squares the tile shows.
    signals.sort_by_key(|s| match s.status {
        SignalStatus::Bad => 0,
        SignalStatus::Watch => 1,
        SignalStatus::Good => 2,
        SignalStatus::Info => 3,
    });
    let graded = signals.iter().filter(|s| s.status != SignalStatus::Info).count();
    let bad = signals.iter().filter(|s| s.status == SignalStatus::Bad).count();
    let watch = signals.iter().filter(|s| s.status == SignalStatus::Watch).count();
    let mut status = if bad > 0 {
        TileStatus::Bad
    } else if watch > 0 {
        TileStatus::Watch
    } else if graded > 0 {
        TileStatus::Good
    } else {
        TileStatus::Info
    };
    let mut takeaway = match signals.first().filter(|s| matches!(s.status, SignalStatus::Bad | SignalStatus::Watch)) {
        Some(worst) => match &worst.healthy {
            Some(healthy) => t!(
                "view.dashboard.health.worst_healthy",
                signal = &worst.label,
                value = signal_value(worst),
                healthy = healthy.trim_end_matches('.')
            ),
            None => t!("view.dashboard.health.worst", signal = &worst.label, value = signal_value(worst)),
        },
        None if graded == 1 => t!("view.dashboard.health.one_healthy"),
        None if graded > 0 => t!("view.dashboard.health.all_healthy", count = graded.to_string()),
        None => t!("view.dashboard.health.ungraded", n = signals.len(), count = signals.len().to_string()),
    };
    let mut items: Vec<TileItem> = Vec::new();
    // An old open transaction holds back cleanup on every engine; it earns a row of its own.
    if let Some(tx) = p.oldest_transaction.as_ref() {
        let assessed = report.findings.iter().find(|f| f.check == "held-cleanup" && f.is_actionable());
        let tx_status = assessed.map(|f| severity_status(f.severity)).unwrap_or(TileStatus::Info);
        let mut item = TileItem::new(
            ItemKind::Signal,
            culprit_core::signal_id::OLDEST_TRANSACTION,
            t!("view.dashboard.health.oldest_transaction"),
        );
        let open = format::duration(tx.seconds * 1000.0);
        item.value = Some(match tx.state_words() {
            Some(state) => t!("view.dashboard.health.oldest_transaction_state", duration = open, state = state),
            None => open,
        });
        item.status = Some(tx_status);
        if let Some(assessed) = assessed
            && tx_status >= TileStatus::Watch
            && status < tx_status
        {
            status = tx_status;
            takeaway = format!("{}.", assessed.title.trim_end_matches('.'));
        }
        items.push(item);
    }
    items.extend(signals.iter().take(8).map(|s| {
        let mut item = TileItem::new(ItemKind::Signal, s.key.clone(), s.label.clone());
        item.value = Some(signal_value(s));
        item.status = Some(signal_status(s.status));
        item
    }));
    let mut tile = Tile::new(TileKey::Health, status, takeaway);
    let more = (bad + watch).saturating_sub(1);
    tile.detail = if more > 0 {
        Some(t!("view.dashboard.health.more", n = more, count = more.to_string()))
    } else {
        Some(t!("view.dashboard.health.periods"))
    };
    let healthy = graded - bad - watch;
    tile.metrics = vec![
        TileMetric::new("bad", &t!("view.dashboard.health.metric.bad"), Some(bad as f64), bad.to_string()),
        TileMetric::new("watch", &t!("view.dashboard.health.metric.watch"), Some(watch as f64), watch.to_string()),
        TileMetric::new(
            "healthy",
            &t!("view.dashboard.health.metric.healthy"),
            Some(healthy as f64),
            healthy.to_string(),
        ),
    ];
    tile.items = items;
    operational(report, tile)
}

fn operational(report: &Report, mut tile: Tile) -> Tile {
    let findings: Vec<_> = report
        .findings
        .iter()
        .filter(|f| {
            matches!(
                f.check.as_str(),
                "storage-capacity"
                    | "sequence-capacity"
                    | "multixact-age"
                    | "operational-errors"
                    | "cluster-health"
                    | "sqlite-wal"
            )
        })
        .collect();
    for f in findings.iter().take(3) {
        let status = if f.is_actionable() { severity_status(f.severity) } else { TileStatus::Info };
        if status >= TileStatus::Watch && status > tile.status {
            tile.status = status;
            tile.takeaway = f.title.clone();
        }
        let mut item = TileItem::new(ItemKind::Finding, f.id.clone(), f.title.clone());
        item.finding_id = Some(f.id.clone());
        item.status = Some(status);
        tile.items.push(item);
    }
    tile
}
