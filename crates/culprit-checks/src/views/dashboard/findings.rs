//! The dashboard's findings tile.

use super::*;

pub(super) fn findings(report: &Report) -> Tile {
    let active: Vec<&Finding> = report.findings.iter().filter(|f| f.is_actionable()).collect();
    let count = |sev: Severity| active.iter().filter(|f| f.severity == sev).count();
    let (high, medium, low) = (count(Severity::High), count(Severity::Medium), count(Severity::Low));
    let stopped = report.findings.iter().filter(|f| f.inactive).count();
    let context = report.findings.iter().filter(|f| !f.inactive && !f.is_actionable()).count();
    let span = report.snapshot.workload.as_ref().map(window_phrase);
    let status = if high > 0 {
        TileStatus::Bad
    } else if medium > 0 {
        TileStatus::Watch
    } else if low > 0 {
        TileStatus::Info
    } else {
        TileStatus::Good
    };
    let top: Vec<&Finding> = active.iter().take(3).copied().collect();
    // The same number the findings summary states: the top three's distinct queries, each counted once.
    let touched = top_findings_share(&report.snapshot, active.iter().copied()).unwrap_or(0.0);
    let (n, count) = (active.len(), active.len().to_string());
    let takeaway = if active.is_empty() {
        match &span {
            Some(span) => t!("view.dashboard.findings.nothing_over", span = span),
            None => t!("view.dashboard.findings.nothing"),
        }
    } else if let Some(cost) =
        crate::support::top_findings_cost(&report.snapshot, active.iter().copied()).filter(|_| touched >= 0.05)
    {
        t!("view.dashboard.findings.top_cost", n = n, count = &count, cost = cost)
    } else if touched >= 0.05 && top.len() == 1 {
        t!("view.dashboard.findings.top_one_share", n = n, count = &count, share = format::percent(touched))
    } else if touched >= 0.05 {
        t!(
            "view.dashboard.findings.top_share",
            n = n,
            count = &count,
            top = top.len().to_string(),
            share = format::percent(touched)
        )
    } else if high > 0 {
        t!("view.dashboard.findings.high", n = n, count = &count, high = high.to_string())
    } else {
        t!("view.dashboard.findings.none_high", n = n, count = &count)
    };
    let mut tile = Tile::new(TileKey::Findings, status, takeaway);
    tile.metrics = vec![
        TileMetric::new("high", &t!("view.dashboard.findings.metric.high"), Some(high as f64), high.to_string()),
        TileMetric::new(
            "medium",
            &t!("view.dashboard.findings.metric.medium"),
            Some(medium as f64),
            medium.to_string(),
        ),
        TileMetric::new("low", &t!("view.dashboard.findings.metric.low"), Some(low as f64), low.to_string()),
    ];
    if context > 0 {
        tile.metrics.push(TileMetric::new(
            "context",
            &t!("view.dashboard.findings.metric.context"),
            Some(context as f64),
            context.to_string(),
        ));
    }
    if stopped > 0 {
        tile.metrics.push(TileMetric::new(
            "stopped",
            &t!("view.dashboard.findings.metric.stopped"),
            Some(stopped as f64),
            stopped.to_string(),
        ));
        // Unchanged since Culprit's first scan, that scan is only the latest they can have run: they may have stopped
        // long before it, and the deploy that changed them may be an earlier one.
        let by_first_scan = report.snapshot.workload.as_ref().is_some_and(|w| {
            report
                .findings
                .iter()
                .filter(|f| f.inactive)
                .all(|f| f.last_seen.as_deref().is_some_and(|last| crate::refine::last_ran_by_first_scan(w, last)))
        });
        // One statement behind them all is "a query … it", however many findings rest on it.
        let behind = crate::summary::Stopped::of(&report.findings);
        let one = behind.statements + behind.unnamed == 1;
        let count = stopped.to_string();
        tile.detail = Some(match (one, by_first_scan) {
            (true, true) => t!("view.dashboard.findings.stopped.one_by_first_scan", n = stopped, count = count),
            (false, true) => t!("view.dashboard.findings.stopped.many_by_first_scan", n = stopped, count = count),
            (true, false) => t!("view.dashboard.findings.stopped.one", n = stopped, count = count),
            (false, false) => t!("view.dashboard.findings.stopped.many", n = stopped, count = count),
        });
    }
    tile.items = top
        .iter()
        .map(|f| {
            let mut item = TileItem::new(ItemKind::Finding, f.id.clone(), f.title.clone());
            item.value = Some(f.impact.label.clone());
            item.share = f.impact.db_time_share;
            item.status = Some(severity_status(f.severity));
            item.finding_id = Some(f.id.clone());
            item
        })
        .collect();
    tile
}
