//! The report's summary: counts by category and severity, the share of database time the top findings touch, and the
//! headline sentence.

use culprit_core::{Category, CategoryCount, Finding, Severity, SeverityCount, Snapshot, Summary, format, t};

use crate::support;

/// The report's summary for these findings: counts, the share the top ones touch and the headline.
pub fn summarize(snapshot: &Snapshot, findings: &[Finding]) -> Summary {
    let mut by_category: Vec<CategoryCount> = Vec::new();
    for category in [Category::Indexes, Category::Queries, Category::Maintenance, Category::Configuration] {
        let count = findings.iter().filter(|f| f.is_actionable() && f.category == category).count() as u32;
        if count > 0 {
            by_category.push(CategoryCount { category, count });
        }
    }
    let by_severity = [Severity::High, Severity::Medium, Severity::Low]
        .into_iter()
        .map(|severity| SeverityCount {
            severity,
            count: findings.iter().filter(|f| f.is_actionable() && f.severity == severity).count() as u32,
        })
        .filter(|c| c.count > 0)
        .collect();

    let top_share = support::top_findings_share(snapshot, findings.iter());
    // Over Culprit's scans where they measured every statement behind it, as the findings say it.
    let scans = support::top_findings_scans(snapshot, findings.iter());

    let cost = support::top_findings_cost(snapshot, findings.iter());
    let n = findings.iter().filter(|f| f.is_actionable()).count();
    let window = support::summary_window(snapshot);
    // The headline's sentences, joined.
    let mut headline = Vec::new();
    let opportunities = || t!("summary.headline.opportunities", n = n, count = n.to_string());
    match (n, top_share, &window) {
        (0, _, Some(window)) => headline.push(t!("summary.headline.nothing_over", window = window)),
        // No statements were examined: say what was.
        (0, _, None) if snapshot.workload.is_none() => headline.push(t!("summary.headline.nothing_without_history")),
        (0, _, None) => headline.push(t!("summary.headline.nothing")),
        (_, Some(share), Some(window)) if share >= 0.05 => {
            headline.push(opportunities());
            headline.push(match cost {
                Some(cost) => t!("summary.headline.top_cost", cost = cost),
                None => t!(
                    "summary.headline.top_share",
                    share = format::percent(share),
                    window = scans.as_deref().unwrap_or(window)
                ),
            });
        }
        _ => headline.push(opportunities()),
    }
    let stopped = Stopped::of(findings);
    let (statements, findings_on) = (stopped.statements, stopped.unnamed);
    match (statements, findings_on) {
        (0, 0) => {}
        (s, 0) => headline.push(t!("summary.headline.stopped_statements", n = s, count = s.to_string())),
        (0, u) => headline.push(t!("summary.headline.stopped_findings", n = u, count = u.to_string())),
        (s, u) => headline.push(t!(
            "summary.headline.stopped_both",
            s = s,
            statements = s.to_string(),
            u = u,
            findings = u.to_string()
        )),
    }
    let headline = headline.join(" ");
    Summary {
        opportunities: n as u32,
        by_category,
        by_severity,
        top_share,
        headline,
        no_workload: snapshot.no_workload_reason(),
        runs_said: runs_said(snapshot),
        over_scans: over_scans(snapshot),
        index_notes: crate::indexes::notes::index_notes(snapshot, findings),
        index_counting: crate::indexes::notes::index_counting(snapshot),
        foreign_key_indexes: crate::indexes::notes::foreign_key_indexes(snapshot),
    }
}

/// What Culprit's scans measured of the statements they did (`Summary::over_scans`), as every surface states it.
fn over_scans(snapshot: &Snapshot) -> Vec<culprit_core::StatementCost> {
    let Some(w) = snapshot.workload.as_ref() else { return Vec::new() };
    w.queries
        .iter()
        .filter(|q| w.scans_of(q).is_some() || w.observed_activity(q).is_some())
        .map(|q| w.cost_of(q))
        .collect()
}

/// The statements whose runs a rate a day would misstate (`Summary::runs_said`), said as the brief says them: a burst
/// by when it ran, fewer runs than the window has days as a count.
fn runs_said(snapshot: &Snapshot) -> Vec<culprit_core::RunsSaid> {
    let Some(w) = snapshot.workload.as_ref() else { return Vec::new() };
    w.queries
        .iter()
        .filter(|q| {
            w.cadence(q).is_burst()
                || (w.measurement_hours() >= culprit_core::PER_DAY_MIN_HOURS && w.runs_per_day_of(q).is_none())
        })
        .map(|q| culprit_core::RunsSaid { query: q.id.clone(), runs: w.cost_of(q).runs })
        .collect()
}

/// The statements behind findings that stopped running, counted as statements: one statement with a heavy-query and an
/// unstable-query finding is one. A finding that names none (an index the optimizer stopped asking for) is counted on
/// its own.
pub(crate) struct Stopped {
    /// Distinct statements the inactive findings name.
    pub statements: usize,
    /// Inactive findings that name no statement.
    pub unnamed: usize,
}

impl Stopped {
    pub(crate) fn of(findings: &[Finding]) -> Stopped {
        let inactive: Vec<&Finding> = findings.iter().filter(|f| f.inactive).collect();
        let mut ids: Vec<&str> = inactive.iter().flat_map(|f| f.queries.iter().map(|r| r.id.as_str())).collect();
        ids.sort_unstable();
        ids.dedup();
        Stopped { statements: ids.len(), unnamed: inactive.iter().filter(|f| f.queries.is_empty()).count() }
    }
}

#[cfg(test)]
mod window_tests {
    use culprit_core::Engine;

    use super::summarize;
    use crate::waits::snapshot_with;

    /// A scan with `--window 6h` reads about six hours of Query Store intervals, and the headline says so.
    #[test]
    fn the_headline_names_an_hours_window() {
        let mut snapshot = snapshot_with(Engine::Mssql);
        let workload = snapshot.workload.as_mut().expect("fixture has a workload");
        workload.windowed = true;
        workload.window_hours = 6.0;
        assert_eq!(summarize(&snapshot, &[]).headline, "Nothing stands out over the last 6 hours.");
    }

    /// Query Store keeps whole hourly intervals, so `--window 6h` reads the interval it starts in too: the headline names
    /// the window asked for, and the rounding.
    #[test]
    fn the_headline_names_the_window_asked_for_and_the_rounding() {
        let mut snapshot = snapshot_with(Engine::Mssql);
        snapshot.options = Some(culprit_core::SnapshotOptions::default().with_window_minutes(360));
        let workload = snapshot.workload.as_mut().expect("fixture has a workload");
        workload.windowed = true;
        workload.window_hours = 6.9;
        assert_eq!(
            summarize(&snapshot, &[]).headline,
            "Nothing stands out over the last 6 hours (Query Store keeps whole intervals, so this covers 7)."
        );
        // Rounding that doesn't show isn't mentioned.
        snapshot.workload.as_mut().unwrap().window_hours = 6.3;
        assert_eq!(summarize(&snapshot, &[]).headline, "Nothing stands out over the last 6 hours.");
        // Less data than asked for: what it covers.
        snapshot.workload.as_mut().unwrap().window_hours = 2.0;
        assert_eq!(summarize(&snapshot, &[]).headline, "Nothing stands out over the last 2 hours.");
    }

    /// Every headline, word for word, as English has always said it: how many opportunities, what the top ones touch,
    /// and the findings on statements that stopped running.
    #[test]
    fn every_headline_reads_as_english_writes_it() {
        let snapshot = snapshot_with(Engine::Mssql);
        let q = |i: usize| snapshot.queries()[i].id.clone();
        let on = |id: &str, queries: &[String], inactive: bool| {
            let mut f = crate::support::finding(
                "heavy-query",
                id,
                culprit_core::Category::Queries,
                crate::support::subject(culprit_core::SubjectKind::Query, id),
            );
            f.queries =
                queries.iter().map(|q| culprit_core::QueryRef { id: q.clone(), plan_node: None, note: None }).collect();
            f.inactive = inactive;
            f
        };
        let said = |findings: &[culprit_core::Finding]| summarize(&snapshot, findings).headline;
        let active = on("a", &[q(0)], false);
        let other = on("b", &[q(1)], false);
        let stopped = on("c", &[q(2)], true);
        let stopped_too = on("d", &[q(3)], true);
        let unnamed = on("e", &[], true);
        let unnamed_too = on("f", &[], true);
        let top = "The top ones touch 22.2 s of database time over the last 5 hours (54%).";
        for (findings, want) in [
            (vec![active.clone()], format!("1 opportunity. {top}")),
            (
                vec![active.clone(), other.clone()],
                "2 opportunities. The top ones touch 28.9 s of database time over the last 5 hours (71%).".into(),
            ),
            (
                vec![active.clone(), stopped.clone()],
                format!("1 opportunity. {top} 1 more statement stopped running: a deploy may have changed it."),
            ),
            (
                vec![active.clone(), stopped.clone(), stopped_too.clone()],
                format!("1 opportunity. {top} 2 more statements stopped running: a deploy may have changed them."),
            ),
            (
                vec![active.clone(), unnamed.clone()],
                format!(
                    "1 opportunity. {top} 1 more finding rests on statements that stopped running: a deploy may have \
                     changed them."
                ),
            ),
            (
                vec![active.clone(), unnamed.clone(), unnamed_too.clone()],
                format!(
                    "1 opportunity. {top} 2 more findings rest on statements that stopped running: a deploy may have \
                     changed them."
                ),
            ),
            (
                vec![active.clone(), stopped.clone(), unnamed.clone()],
                format!(
                    "1 opportunity. {top} 1 more statement stopped running, and 1 more finding rests on statements \
                     that did: a deploy may have changed them."
                ),
            ),
            (
                vec![stopped.clone(), stopped_too.clone(), unnamed.clone(), unnamed_too.clone()],
                "Nothing stands out over the last 5 hours. 2 more statements stopped running, and 2 more findings rest \
                 on statements that did: a deploy may have changed them."
                    .into(),
            ),
        ] {
            assert_eq!(said(&findings), want);
        }
        // Shares of entries cached at different times are no amount over one period: the share and its window.
        let mut cached = snapshot.clone();
        cached.workload.as_mut().unwrap().hourly_time_ms = Some(1_000.0);
        assert_eq!(
            summarize(&cached, std::slice::from_ref(&active)).headline,
            "1 opportunity. The top ones touch 54% of database time over the last 5 hours."
        );
        let mut none = snapshot.clone();
        none.workload = None;
        assert_eq!(
            summarize(&none, &[]).headline,
            "Nothing stands out in the tables, indexes and settings; this scan has no query history."
        );
        assert_eq!(summarize(&none, std::slice::from_ref(&active)).headline, "1 opportunity.");
    }
}
