//! The dashboard's right now tile.

use super::*;

pub(super) fn not_read(s: &Snapshot) -> Tile {
    if !s.server.engine.has_server() {
        let mut tile = Tile::new(
            TileKey::Now,
            TileStatus::Unavailable,
            t!("view.dashboard.now.serverless", engine = s.server.engine.to_string()),
        );
        tile.detail = Some(t!("view.dashboard.now.serverless_detail"));
        return tile;
    }
    let mut tile = Tile::new(TileKey::Now, TileStatus::Info, t!("view.dashboard.now.not_read"));
    tile.detail = Some(t!("view.dashboard.now.not_read_detail"));
    tile
}

pub(super) fn idle_in_transaction(s: &culprit_core::Session) -> bool {
    s.transaction_ms.is_some() && (s.state.starts_with("idle in transaction") || s.state == "sleeping")
}

pub(super) fn running(s: &culprit_core::Session) -> bool {
    s.state != "idle" && s.state != "sleeping" && !s.state.starts_with("idle in transaction")
}

/// Sessions right now: blocking first, then an idle open transaction, then what runs. Desktop polling consumes this
/// same assessment through `Workspace::dashboard_live`.
pub fn now(live: &LiveState) -> Tile {
    let active: Vec<&culprit_core::Session> = live.sessions.iter().filter(|s| running(s)).collect();
    let blocked = live.sessions.iter().filter(|s| s.blocked_by.is_some()).count();
    let idle_tx: Vec<&culprit_core::Session> = live.sessions.iter().filter(|s| idle_in_transaction(s)).collect();
    let longest = active
        .iter()
        .filter(|s| s.blocked_by.is_none())
        .max_by(|a, b| a.elapsed_ms.unwrap_or(0.0).total_cmp(&b.elapsed_ms.unwrap_or(0.0)));
    let oldest_idle =
        idle_tx.iter().max_by(|a, b| a.transaction_ms.unwrap_or(0.0).total_cmp(&b.transaction_ms.unwrap_or(0.0)));
    let chains = live.blocking.as_ref().map(|b| b.chains.as_slice()).unwrap_or_default();

    let (status, takeaway) = if let Some(chain) = chains.first() {
        let status = blocking_status(chain.waiters as usize, chain.longest_wait_ms);
        (status, chain.headline.clone())
    } else if blocked > 0 {
        (
            blocking_status(
                blocked,
                live.sessions
                    .iter()
                    .filter(|s| s.blocked_by.is_some())
                    .filter_map(|s| s.wait_ms)
                    .max_by(f64::total_cmp),
            ),
            t!("view.dashboard.now.blocked", n = blocked, count = blocked.to_string()),
        )
    } else if let Some(s) = oldest_idle.filter(|s| s.transaction_ms.unwrap_or(0.0) >= 600_000.0) {
        (
            TileStatus::Watch,
            t!(
                "view.dashboard.now.idle_transaction",
                session = &s.id,
                duration = format::duration(s.transaction_ms.unwrap_or(0.0))
            ),
        )
    } else if live.blocking_incomplete() {
        (TileStatus::Info, t!("view.dashboard.now.incomplete"))
    } else if active.is_empty() {
        (TileStatus::Good, t!("view.dashboard.now.quiet"))
    } else {
        let (n, count) = (active.len(), active.len().to_string());
        let takeaway = match longest.and_then(|s| s.elapsed_ms) {
            Some(ms) if ms >= 1_000.0 => {
                t!("view.dashboard.now.running_longest", n = n, count = count, duration = format::duration(ms))
            }
            _ => t!("view.dashboard.now.running", n = n, count = count),
        };
        (TileStatus::Good, takeaway)
    };
    let mut tile = Tile::new(TileKey::Now, status, takeaway);
    let long_idle = idle_tx.iter().filter(|s| s.transaction_ms.is_some_and(|ms| ms >= 600_000.0)).count();
    tile.metrics = vec![
        TileMetric::new(
            "active",
            &t!("view.dashboard.now.metric.running"),
            Some(active.len() as f64),
            active.len().to_string(),
        ),
        TileMetric::new(
            "blocked",
            &t!("view.dashboard.now.metric.blocked"),
            (!live.blocking_incomplete()).then_some(blocked as f64),
            if live.blocking_incomplete() {
                t!("view.dashboard.now.at_least", count = blocked.to_string())
            } else {
                blocked.to_string()
            },
        ),
        TileMetric::new(
            "idle-in-transaction",
            &t!("view.dashboard.now.metric.idle"),
            Some(idle_tx.len() as f64),
            idle_tx.len().to_string(),
        ),
        TileMetric::new(
            "long-idle-in-transaction",
            &t!("view.dashboard.now.metric.long_idle"),
            Some(long_idle as f64),
            long_idle.to_string(),
        ),
        TileMetric::new(
            "sessions",
            &t!("view.dashboard.now.metric.sessions"),
            Some(live.sessions.len() as f64),
            live.sessions.len().to_string(),
        ),
    ];
    let mut items: Vec<TileItem> = chains
        .iter()
        .take(3)
        .map(|c| {
            let mut item = TileItem::new(ItemKind::Session, c.head.clone(), c.headline.clone());
            item.value = Some(t!("view.dashboard.now.waiting", count = c.waiters.to_string()));
            item.status = Some(blocking_status(c.waiters as usize, c.longest_wait_ms));
            item
        })
        .collect();
    let mut longest_first = active.clone();
    longest_first.sort_by(|a, b| b.elapsed_ms.unwrap_or(0.0).total_cmp(&a.elapsed_ms.unwrap_or(0.0)));
    items.extend(longest_first.iter().filter(|s| !chains.iter().any(|c| c.head == s.id)).take(3).map(|s| {
        let label = s
            .sql
            .as_deref()
            .map(statement_handle)
            .unwrap_or_else(|| s.command.clone().unwrap_or_else(|| s.state.clone()));
        let mut item = TileItem::new(
            ItemKind::Session,
            s.id.clone(),
            t!("view.dashboard.now.session", session = &s.id, label = label),
        );
        item.value = s.elapsed_ms.map(format::duration);
        item.status = Some(if s.blocked_by.is_some() { blocking_status(1, s.wait_ms) } else { TileStatus::Good });
        item
    }));
    tile.items = items;
    // The time it was read, then what the read couldn't see (a space after it, as ever, when there is nothing).
    let read = t!("view.dashboard.now.read_at", time = culprit_core::changes::short_time(&live.taken_at));
    tile.detail = Some(format!("{read} {}", live.notes.join(" ")));
    tile
}

fn blocking_status(waiters: usize, ms: Option<f64>) -> TileStatus {
    if ms.is_some_and(|ms| ms >= 30_000.0) {
        TileStatus::Bad
    } else if waiters >= 5 || ms.is_some_and(|ms| ms >= 1000.0) {
        TileStatus::Watch
    } else {
        TileStatus::Info
    }
}
