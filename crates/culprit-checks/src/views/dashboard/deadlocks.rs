//! The dashboard's deadlocks tile.

use super::*;

pub(super) fn deadlocks(report: &Report) -> Tile {
    let s = &report.snapshot;
    let family = family(&s.server);
    let Some(d) = &s.deadlocks else {
        if s.server.engine.has_server() && !s.server.engine.spec().abilities().deadlocks {
            let mut tile =
                Tile::new(TileKey::Deadlocks, TileStatus::Unavailable, t!("view.dashboard.deadlocks.unavailable"));
            let refusal = s.server.engine.refused(culprit_core::Ability::Deadlocks);
            tile.detail = Some(match refusal.hint {
                Some(hint) => [refusal.message, hint].join(" "),
                None => refusal.message,
            });
            return tile;
        }
        let engine = s.server.engine.to_string();
        let headline = if s.server.engine.has_server() {
            t!("view.dashboard.deadlocks.not_read")
        } else {
            t!("view.dashboard.deadlocks.serverless", engine = &engine)
        };
        let mut tile = Tile::new(TileKey::Deadlocks, TileStatus::Unavailable, headline);
        tile.detail = Some(match collector_note(s, &[culprit_core::source_id::DEADLOCKS]) {
            Some(note) => note,
            None if !s.server.engine.has_server() => t!("view.dashboard.deadlocks.serverless_detail", engine = &engine),
            None => t!("view.dashboard.deadlocks.scan_again"),
        });
        return tile;
    };
    let finding_id = report.findings.iter().find(|f| f.check == "deadlocks").map(|f| f.id.clone());
    // This database's, as the check counts them: the server-wide reports carry other databases' too.
    let here: Vec<&culprit_core::Deadlock> =
        d.deadlocks.iter().filter(|dl| crate::deadlocks::in_this_database(dl, s)).collect();
    // Only a dated deadlock is "in the last day": InnoDB keeps its latest one however old it is.
    let dated = here.iter().filter(|dl| dl.at.is_some()).count();
    if !here.is_empty() && dated == 0 {
        let mut tile = Tile::new(TileKey::Deadlocks, TileStatus::Info, t!("view.dashboard.deadlocks.undated"));
        tile.detail = d.note.clone().or_else(|| Some(t!("view.dashboard.deadlocks.read_from", source = &d.source)));
        tile.metrics = vec![TileMetric::new(
            "count",
            &t!("view.dashboard.deadlocks.metric.undated"),
            Some(here.len() as f64),
            here.len().to_string(),
        )];
        return tile;
    }
    if !here.is_empty() {
        // Same pattern on the same tables is the same problem.
        let mut groups: BTreeMap<(String, String), Vec<&culprit_core::Deadlock>> = BTreeMap::new();
        for dl in here.iter().filter(|dl| dl.at.is_some()) {
            groups.entry((dl.pattern.label().to_string(), dl.objects.join(", "))).or_default().push(dl);
        }
        let mut groups: Vec<((String, String), Vec<&culprit_core::Deadlock>)> = groups.into_iter().collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
        let n = dated;
        let ((pattern, objects), biggest) = &groups[0];
        let (count, same) = (n.to_string(), biggest.len().to_string());
        let when = last(d.since_hours);
        let takeaway = match (n, objects.is_empty()) {
            (1, true) => t!("view.dashboard.deadlocks.one", when = &when, pattern = pattern),
            (1, false) => t!("view.dashboard.deadlocks.one_on", when = &when, pattern = pattern, objects = objects),
            _ if biggest.len() == n && objects.is_empty() => {
                t!("view.dashboard.deadlocks.all", count = &count, when = &when, pattern = pattern)
            }
            _ if biggest.len() == n => t!(
                "view.dashboard.deadlocks.all_on",
                count = &count,
                when = &when,
                pattern = pattern,
                objects = objects
            ),
            _ if groups.len() == n => t!("view.dashboard.deadlocks.each_different", count = &count, when = &when),
            (_, true) => {
                t!("view.dashboard.deadlocks.some", count = &count, when = &when, same = &same, pattern = pattern)
            }
            (_, false) => t!(
                "view.dashboard.deadlocks.some_on",
                count = &count,
                when = &when,
                same = &same,
                pattern = pattern,
                objects = objects
            ),
        };
        // Bad where the check says High: by how often, not how many.
        let status = report
            .findings
            .iter()
            .filter(|f| f.check == "deadlocks" && f.is_actionable())
            .map(|f| severity_status(f.severity))
            .max()
            .unwrap_or(TileStatus::Info);
        let mut tile = Tile::new(TileKey::Deadlocks, status, takeaway);
        tile.detail = d.note.clone().or_else(|| Some(t!("view.dashboard.deadlocks.read_from", source = &d.source)));
        tile.metrics =
            vec![TileMetric::new("count", &t!("view.dashboard.deadlocks.metric.count"), Some(n as f64), n.to_string())];
        tile.items = groups
            .iter()
            .take(3)
            .map(|((pattern, objects), list)| {
                let label = if objects.is_empty() {
                    capitalized(pattern)
                } else {
                    t!("view.dashboard.deadlocks.item_on", pattern = capitalized(pattern), objects = objects)
                };
                let mut item = TileItem::new(ItemKind::Deadlock, list[0].id.clone(), label);
                item.value = Some(t!("view.dashboard.deadlocks.occurrences", count = list.len().to_string()));
                item.at = list.iter().filter_map(|dl| dl.at.clone()).max();
                item.status = Some(TileStatus::Watch);
                item.finding_id = finding_id.clone();
                item
            })
            .collect();
        return tile;
    }
    if let Some(counted) = d.counted {
        let since = d.counted_since.as_deref().and_then(|from| format::age_between(from, &s.taken_at));
        let count = format::count(counted);
        let mut tile = if counted > 0.0 {
            // A total with no reset on record is from an unknown span: said, not graded.
            Tile::new(
                TileKey::Deadlocks,
                report
                    .findings
                    .iter()
                    .filter(|f| f.check == "deadlocks" && f.is_actionable())
                    .map(|f| severity_status(f.severity))
                    .max()
                    .unwrap_or(TileStatus::Info),
                match &since {
                    Some(age) => t!(
                        "view.dashboard.deadlocks.counted_since",
                        n = counted,
                        family = family,
                        count = &count,
                        age = age
                    ),
                    None => t!("view.dashboard.deadlocks.counted", n = counted, family = family, count = &count),
                },
            )
        } else {
            Tile::new(
                TileKey::Deadlocks,
                TileStatus::Good,
                match &since {
                    Some(age) => t!("view.dashboard.deadlocks.none_counted_since", age = age),
                    None => t!("view.dashboard.deadlocks.none_counted"),
                },
            )
        };
        tile.detail = d.note.clone();
        tile.metrics =
            vec![TileMetric::new("counted", &t!("view.dashboard.deadlocks.metric.counted"), Some(counted), count)];
        return tile;
    }
    let mut tile = Tile::new(
        TileKey::Deadlocks,
        TileStatus::Good,
        t!("view.dashboard.deadlocks.none", when = last(d.since_hours)),
    );
    tile.detail = d.note.clone();
    tile.metrics = vec![TileMetric::new("count", &t!("view.dashboard.deadlocks.metric.count"), Some(0.0), "0")];
    tile
}
