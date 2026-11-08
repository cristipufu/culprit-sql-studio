//! The dashboard's indexes tile.

use super::*;

pub(super) fn indexes(report: &Report) -> Tile {
    let advice = crate::advisor::index_advice(report);
    // A query that asks for two indexes still only takes its share of time once. A foreign key's index is for deletes on
    // the parent: the queries that merely name the key wouldn't necessarily get faster, so they don't count here.
    let mut seen = HashSet::new();
    let mut share = 0.0;
    let measured = advice.candidates.iter().filter(|c| c.source != culprit_core::CandidateSource::ForeignKey);
    for q in measured.flat_map(|c| &c.queries) {
        if seen.insert(q.id.as_str()) {
            share += q.share.unwrap_or(0.0);
        }
    }
    let share = share.min(1.0);
    let adds = advice.candidates.len();
    let cleanups = advice.cleanups.len();
    let reclaim: f64 = advice.cleanups.iter().filter_map(|c| c.size_bytes).sum();
    let size = (reclaim > 0.0).then(|| format::bytes(reclaim));
    // New indexes come from what queries need: without query history, none can be suggested, which says nothing about
    // whether any is needed.
    let history = report.snapshot.workload.is_some();
    let (a, c) = (adds.to_string(), cleanups.to_string());
    let takeaway = match (adds, cleanups) {
        (0, 0) if !history => t!("view.dashboard.indexes.none_no_history"),
        (0, n) if !history => match &size {
            Some(size) => t!("view.dashboard.indexes.cleanup_no_history_sized", n = n, count = &c, size = size),
            None => t!("view.dashboard.indexes.cleanup_no_history", n = n, count = &c),
        },
        (0, 0) => t!("view.dashboard.indexes.nothing"),
        (0, n) => match &size {
            Some(size) => t!("view.dashboard.indexes.cleanup_only_sized", n = n, count = &c, size = size),
            None => t!("view.dashboard.indexes.cleanup_only", n = n, count = &c),
        },
        (n, cleanups) => {
            // What can go, said at the end of the sentence about what to add: "2 indexes can go (27.1 MB)".
            let can_go = match &size {
                _ if cleanups == 0 => None,
                Some(size) => Some(t!("view.dashboard.indexes.can_go_sized", n = cleanups, count = &c, size = size)),
                None => Some(t!("view.dashboard.indexes.can_go", n = cleanups, count = &c)),
            };
            let queries = seen.len();
            match can_go {
                None if share >= 0.01 => {
                    t!("view.dashboard.indexes.add_share", n = n, count = &a, share = format::percent(share))
                }
                None if queries > 0 => t!(
                    "view.dashboard.indexes.add_queries",
                    n = n,
                    count = &a,
                    queries = t!("view.dashboard.indexes.queries", n = queries, count = queries.to_string())
                ),
                None => t!("view.dashboard.indexes.add_worth", n = n, count = &a),
                Some(can_go) if share >= 0.01 => t!(
                    "view.dashboard.indexes.add_share_cleanup",
                    n = n,
                    count = &a,
                    share = format::percent(share),
                    cleanup = can_go
                ),
                Some(can_go) if queries > 0 => t!(
                    "view.dashboard.indexes.add_queries_cleanup",
                    n = n,
                    count = &a,
                    queries = t!("view.dashboard.indexes.queries", n = queries, count = queries.to_string()),
                    cleanup = can_go
                ),
                Some(can_go) => t!("view.dashboard.indexes.add_worth_cleanup", n = n, count = &a, cleanup = can_go),
            }
        }
    };
    let status = if adds > 0 && share >= 0.1 {
        TileStatus::Watch
    } else if adds + cleanups > 0 || !history {
        TileStatus::Info
    } else {
        TileStatus::Good
    };
    let mut tile = Tile::new(TileKey::Indexes, status, takeaway);
    tile.detail = advice.notes.first().cloned().or_else(|| {
        (!advice.counters.is_empty()).then(|| t!("view.dashboard.indexes.counted", period = &advice.counters))
    });
    tile.metrics = vec![
        TileMetric::new("add", &t!("view.dashboard.indexes.metric.add"), Some(adds as f64), adds.to_string()),
        TileMetric::new(
            "cleanup",
            &t!("view.dashboard.indexes.metric.cleanup"),
            Some(cleanups as f64),
            cleanups.to_string(),
        ),
        TileMetric::new("reclaim", &t!("view.dashboard.indexes.metric.reclaim"), Some(reclaim), format::bytes(reclaim)),
        TileMetric::new(
            "tables",
            &t!("view.dashboard.indexes.metric.tables"),
            Some(advice.tables.len() as f64),
            advice.tables.len().to_string(),
        ),
    ];
    let mut items: Vec<TileItem> = advice
        .candidates
        .iter()
        .take(3)
        .map(|c| {
            let mut item = TileItem::new(
                ItemKind::IndexAdd,
                format!("{}.{}({})", c.schema, c.table, c.keys.join(", ")),
                format!("{} ({})", c.table, c.keys.join(", ")),
            );
            item.value = c.db_time_share.map(format::percent);
            item.share = c.db_time_share;
            item.query_id = c.queries.first().map(|q| q.id.clone());
            item.finding_id = c.finding_id.clone();
            item
        })
        .collect();
    items.extend(advice.cleanups.iter().take(2).map(|c| {
        let mut item = TileItem::new(
            ItemKind::IndexCleanup,
            format!("{}.{}.{}", c.schema, c.table, c.index),
            t!("view.dashboard.indexes.cleanup_item", index = &c.index, table = &c.table),
        );
        item.value = c.size_bytes.map(format::bytes);
        item.finding_id = c.finding_id.clone();
        item
    }));
    tile.items = items;
    tile
}
