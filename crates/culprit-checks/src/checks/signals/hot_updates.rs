//! The `hot-updates` check: Tables where few updates are HOT (heap-only): each other update writes a new entry in every index and leaves cleanup for VACUUM. Says whether pages are too full (fillfactor) or an indexed column changes (PostgreSQL 16+ tells them apart).

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn hot_updates_info() -> CheckInfo {
    CheckInfo {
        id: "hot-updates".into(),
        title: t!("check.hot_updates.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres],
        requires: vec![],
        description: t!("check.hot_updates.info.description"),
    }
}

check!(
    HotUpdatesCheck: hot_updates_info,
    decide = cold_updates,
    render = hot_updates_finding,
    case<'s> = ColdUpdates<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Tables(crate::evidence::TableMeasure::HotUpdates)],
);

/// Non-HOT updates a day to one table that make hot-updates Medium.
pub(super) const COLD_UPDATES_MEDIUM_PER_DAY: f64 = 1_000_000.0;

/// A table with indexes where fewer than half of 10,000 or more updates were HOT.
pub struct ColdUpdates<'s> {
    table: &'s TableStat,
    updates: f64,
    hot: f64,
    ratio: f64,
    indexes: usize,
    fillfactor: f64,
    /// The share of the non-HOT updates that moved to another page for lack of room (PostgreSQL 16+).
    moved: Option<f64>,
    /// Foreign keys that reference the table, and the child tables they are on.
    referencing: usize,
    children: Vec<String>,
    severity: Severity,
    /// Without a rate high enough to grade, the caveat on how long the counts have run: `Some(None)` when that isn't
    /// known.
    counted_for: Option<Option<f64>>,
    /// Leaving room on the page is the fix (the pages are full, or the cause isn't known and fillfactor is 100).
    room: bool,
}

fn cold_updates(snapshot: &Snapshot) -> Vec<ColdUpdates<'_>> {
    let mut cases: Vec<ColdUpdates<'_>> = snapshot
        .tables
        .iter()
        .filter_map(|t| {
            let s = t.storage.as_ref()?;
            let updates = t.updates.filter(|u| *u >= 10_000.0)?;
            let hot = s.hot_updates?;
            let ratio = hot / updates;
            // A table without indexes has no entries to rewrite: a non-HOT update there costs what a HOT one does.
            let indexes = snapshot.indexes_on(&t.schema, &t.name).count();
            (ratio < 0.5 && indexes > 0).then_some((t, s, updates, hot, ratio, indexes))
        })
        .map(|(t, s, updates, hot, ratio, indexes)| {
            let cold = updates - hot;
            let fillfactor = s.fillfactor.unwrap_or(100.0);
            let moved = s.new_page_updates.map(|n| (n / cold.max(1.0)).min(1.0));
            let referencing: Vec<&culprit_core::ForeignKey> = crate::checks::indexes::enforced_foreign_keys(snapshot)
                .filter(|fk| {
                    fk.ref_schema.eq_ignore_ascii_case(&t.schema) && fk.ref_table.eq_ignore_ascii_case(&t.name)
                })
                .collect();
            let children: Vec<String> = referencing
                .iter()
                .map(|fk| format!("{}.{}", fk.schema, fk.table))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            // The counts run from the database's last statistics reset, which PostgreSQL 15+ leaves empty until the first
            // one: then their age is unknown (the counters outlive restarts), and so is any rate. A one-time backfill
            // keeps the share low long after it ran, so only a measured rate raises it.
            let hours = s.counter_hours.filter(|h| *h >= culprit_core::RATE_MIN_HOURS);
            let per_day = hours.map(|h| cold / h * 24.0);
            let medium_threshold =
                if !referencing.is_empty() { COLD_UPDATES_MEDIUM_PER_DAY / 10.0 } else { COLD_UPDATES_MEDIUM_PER_DAY };
            ColdUpdates {
                table: t,
                updates,
                hot,
                ratio,
                indexes,
                fillfactor,
                moved,
                referencing: referencing.len(),
                children,
                severity: since_start_severity(per_day, f64::INFINITY, medium_threshold),
                counted_for: per_day.is_none_or(|d| d < medium_threshold).then_some(s.counter_hours),
                room: moved.is_none_or(|m| m >= 0.5) && fillfactor >= 100.0,
            }
        })
        .collect();
    // Ranked by severity alone (they carry no statement), keeping their order within one.
    cases.sort_by(|a, b| base_score(b.severity).total_cmp(&base_score(a.severity)));
    cases.truncate(5);
    cases
}

fn hot_updates_finding(_: &Snapshot, cold: ColdUpdates<'_>) -> Finding {
    let t = cold.table;
    let name = t.qualified();
    let mut f = finding("hot-updates", &name, Category::Maintenance, subject(SubjectKind::Table, &name));
    let ratio = format::percent(cold.ratio);
    let fillfactor = format!("{:.0}", cold.fillfactor);
    f.title = if cold.hot < 1.0 {
        t!("check.hot_updates.title.none", table = &t.name)
    } else {
        t!("check.hot_updates.title.some", share = &ratio, table = &t.name)
    };
    let mut summary = vec![if cold.hot < 1.0 {
        t!(
            "check.hot_updates.summary.none_hot",
            table = &t.name,
            updates = format::count(cold.updates),
            indexes = cold.indexes
        )
    } else {
        t!(
            "check.hot_updates.summary.some_hot",
            cold = format::count(cold.updates - cold.hot),
            table = &t.name,
            updates = format::count(cold.updates),
            indexes = cold.indexes
        )
    }];
    summary.push(match cold.moved {
        Some(m) if m >= 0.5 => {
            t!("check.hot_updates.summary.pages_full", share = format::percent(m), fillfactor = &fillfactor)
        }
        Some(m) => t!("check.hot_updates.summary.indexed_column", share = format::percent(m)),
        None => t!("check.hot_updates.summary.either", fillfactor = &fillfactor),
    });
    if cold.referencing > 0 {
        summary.push(t!(
            "check.hot_updates.summary.referenced",
            fks = cold.referencing,
            children = format::list(&cold.children),
            table = &t.name,
            tables = cold.children.len()
        ));
    }
    f.summary = summary.join(" ");
    f.severity = cold.severity;
    match cold.counted_for {
        Some(Some(hours)) => {
            f.caveats.push(t!("check.hot_updates.caveat.counted_for", span = culprit_core::span_words(hours)))
        }
        Some(None) => f.caveats.push(t!("check.hot_updates.caveat.unknown_start")),
        None => {}
    }
    severity_score(&mut f);
    f.impact.set_label(t!("check.hot_updates.impact", share = &ratio));
    f.confidence = if cold.moved.is_some() { Confidence::High } else { Confidence::Medium };
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.hot_updates.metric.updates"), cold.updates, Unit::Count),
        Metric::said(culprit_core::key!("check.hot_updates.metric.hot"), cold.ratio, Unit::Ratio),
        Metric::said(culprit_core::key!("check.hot_updates.metric.indexes"), cold.indexes as f64, Unit::Count),
        Metric::said(culprit_core::key!("check.hot_updates.metric.fillfactor"), cold.fillfactor, Unit::Count),
    ];
    if cold.referencing > 0 {
        f.metrics.push(Metric::said(
            culprit_core::key!("check.hot_updates.metric.referencing"),
            cold.referencing as f64,
            Unit::Count,
        ));
    }
    f.fix = Some(Fix {
        summary: if cold.room {
            t!("check.hot_updates.fix.fillfactor")
        } else {
            t!("check.hot_updates.fix.indexed_column")
        },
        sql: cold
            .room
            .then(|| format!("ALTER TABLE {} SET (fillfactor = 85);", qualified(Engine::Postgres, &t.schema, &t.name))),
        cost: None,
    });
    f.verify = vec![t!("check.hot_updates.verify")];
    f
}
