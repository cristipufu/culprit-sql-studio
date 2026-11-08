//! The `hot-rows` check: Indexes where sessions waited a long time for row and page locks, and where locks escalated to the whole table (sys.dm_db_index_operational_stats). MySQL, MariaDB and PostgreSQL keep no lock waits per table; see lock-time and Live.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::Actionability;
use culprit_core::signals::ContentionBaseline;

pub fn hot_rows_info() -> CheckInfo {
    CheckInfo {
        id: "hot-rows".into(),
        title: t!("check.hot_rows.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.hot_rows.info.description"),
    }
}

check!(
    HotRowsCheck: hot_rows_info,
    decide = contended_tables,
    render = hot_rows_finding,
    case<'s> = ContendedTable<'s>,
    evidence = &[Evidence::Source(culprit_core::source_id::INDEX_CONTENTION)],
);

/// Lock waits a day on one table that make hot-rows High: ten minutes. A minute a day of waits a second or longer each
/// is Medium.
pub(super) const HOT_ROWS_HIGH_MS_PER_DAY: f64 = 600_000.0;
const HOT_ROWS_MEDIUM_MS_PER_DAY: f64 = 60_000.0;

/// A table whose indexes' row and page locks were waited for ten seconds or more in all: one finding per table, since
/// its indexes' waits are the same rows fought over, whichever index the lock was taken on.
pub struct ContendedTable<'s> {
    schema: String,
    table: String,
    /// Its indexes with lock waits, the most waited on first.
    indexes: Vec<&'s IndexStat>,
    waits: f64,
    wait_ms: f64,
    escalations: f64,
    /// The waits since an earlier comparable scan: (that scan, hours since, waits, their time).
    measured: Option<(&'s ContentionBaseline, f64, f64, f64)>,
    /// The comparison lies inside the selected window.
    in_window: bool,
    /// None new since the earlier scan.
    unchanged: bool,
    /// Its time waiting a day, said only over 20 hours or more of comparison.
    per_day: Option<f64>,
    severity: Severity,
    actionability: Actionability,
    /// The statements that wait for locks on it, the longest waiting first.
    locked: Vec<&'s QueryStat>,
    /// READ_COMMITTED_SNAPSHOT is known to be off: the fix offers it for readers.
    rcsi_off: bool,
}

fn contended_tables(snapshot: &Snapshot) -> Vec<ContendedTable<'_>> {
    let rcsi = snapshot.setting("is_read_committed_snapshot_on").map(|s| crate::config::is_on(&s.value));
    let mut by_table: BTreeMap<(String, String), Vec<(&IndexStat, &culprit_core::IndexContention)>> = BTreeMap::new();
    for i in &snapshot.indexes {
        if let Some(c) = i.contention.as_ref().filter(|c| c.lock_waits() >= 1.0 && c.lock_wait_ms() > 0.0) {
            by_table.entry((i.schema.clone(), i.table.clone())).or_default().push((i, c));
        }
    }
    let mut cases: Vec<ContendedTable<'_>> = by_table
        .into_iter()
        .filter_map(|((schema, table), mut indexes)| {
            let waits: f64 = indexes.iter().map(|(_, c)| c.lock_waits()).sum();
            let wait_ms: f64 = indexes.iter().map(|(_, c)| c.lock_wait_ms()).sum();
            (wait_ms >= 10_000.0).then(|| {
                indexes.sort_by(|a, b| b.1.lock_wait_ms().total_cmp(&a.1.lock_wait_ms()));
                (schema, table, indexes, waits, wait_ms)
            })
        })
        .map(|(schema, table, indexes, waits, wait_ms)| {
            let escalations: f64 = indexes.iter().map(|(_, c)| c.lock_escalations).sum();
            let measured = snapshot.contention_before.as_ref().and_then(|b| {
                let hours = days_between(&b.taken_at, &snapshot.taken_at)? * 24.0;
                if hours < 0.0 {
                    return None;
                }
                let delta = snapshot
                    .indexes_on(&schema, &table)
                    .filter(|i| i.contention.is_some())
                    .try_fold((0.0, 0.0), |(n, ms), i| b.since(i).map(|d| (n + d.waits, ms + d.wait_ms)))?;
                if delta == (0.0, 0.0)
                    && let Some(minutes) = snapshot.options.as_ref().map(|o| o.window_in_minutes())
                    && hours < f64::from(minutes) / 60.0 * 0.8
                {
                    return None;
                }
                Some((b, hours, delta.0, delta.1))
            });
            let in_window = measured.is_some_and(|(_, hours, _, _)| {
                snapshot.options.as_ref().is_none_or(|o| hours <= f64::from(o.window_in_minutes()) / 60.0 * 1.25)
            });
            let unchanged = measured.is_some_and(|(_, _, n, ms)| n == 0.0 && ms == 0.0);
            let (shown_waits, shown_ms) = measured.map_or((waits, wait_ms), |(_, _, n, ms)| (n, ms));
            let avg = per_run(shown_ms, shown_waits);
            // Counted since the indexes were loaded, with no time for each wait: graded on a rate measured over an
            // hour or more, never on the total.
            let rate = measured
                .filter(|(_, h, _, _)| in_window && *h >= culprit_core::RATE_MIN_HOURS)
                .map(|(_, h, _, ms)| ms * 24.0 / h);
            let amount = measured
                .filter(|_| in_window)
                .map(|(_, h, _, ms)| if h >= culprit_core::RATE_MIN_HOURS { ms * 24.0 / h } else { ms });
            let severity = match amount {
                Some(r) if r >= HOT_ROWS_HIGH_MS_PER_DAY => Severity::High,
                Some(r) if r >= HOT_ROWS_MEDIUM_MS_PER_DAY && avg >= 1_000.0 => Severity::Medium,
                _ => Severity::Low,
            };
            let actionability = if unchanged {
                Actionability::History
            } else if !in_window {
                Actionability::Information
            } else {
                Actionability::default()
            };
            let mut locked: Vec<&QueryStat> =
                queries_touching(snapshot, &schema, &table).into_iter().filter(|q| lock_ms(q) > 0.0).collect();
            locked.sort_by(|a, b| lock_ms(b).total_cmp(&lock_ms(a)));
            locked.truncate(3);
            ContendedTable {
                schema,
                table,
                indexes: indexes.iter().map(|(i, _)| *i).collect(),
                waits,
                wait_ms,
                escalations,
                measured,
                in_window,
                unchanged,
                per_day: rate.filter(|_| measured.is_some_and(|(_, h, _, _)| h >= culprit_core::PER_DAY_MIN_HOURS)),
                severity,
                actionability,
                locked,
                rcsi_off: rcsi == Some(false),
            }
        })
        .collect();
    // Ranked by severity alone (their statements carry no share here), keeping their order within one.
    cases.sort_by(|a, b| base_score(b.severity).total_cmp(&base_score(a.severity)));
    cases.truncate(5);
    cases
}

fn hot_rows_finding(_: &Snapshot, hot: ContendedTable<'_>) -> Finding {
    let ContendedTable { schema, table, indexes, waits, wait_ms, escalations, measured, .. } = hot;
    let name = format!("{schema}.{table}");
    // The subject is the index most of the waiting was on; the finding is the table's.
    let top = indexes[0];
    let mut f = finding(
        "hot-rows",
        &name,
        Category::Indexes,
        subject(SubjectKind::Index, format!("{}.{}", index_label(top), top.name)),
    );
    f.verification_basis = culprit_core::VerificationBasis::CounterChange;
    let shown_ms = measured.map_or(wait_ms, |(_, _, _, ms)| ms);
    f.title = match measured {
        Some((b, _, _, _)) if hot.unchanged => {
            t!("check.hot_rows.title.unchanged", table = &table, since = crate::queries::short_when(&b.taken_at))
        }
        Some((b, _, n, ms)) => {
            let since = crate::queries::short_when(&b.taken_at);
            if n == 0.0 {
                t!("check.hot_rows.title.grew", table = &table, time = format::duration(ms), since = since)
            } else if n == 1.0 {
                t!("check.hot_rows.title.one", table = &table, time = format::duration(ms), since = since)
            } else {
                t!(
                    "check.hot_rows.title.waits",
                    n = n,
                    waits = format::count(n),
                    table = &table,
                    average = format::duration(per_run(ms, n)),
                    since = since
                )
            }
        }
        None if waits == 1.0 => {
            t!("check.hot_rows.title.one_historical", table = &table, time = format::duration(wait_ms))
        }
        None => t!("check.hot_rows.title.historical", table = &table, n = waits, waits = format::count(waits)),
    };
    let mut summary = Vec::new();
    if let Some((b, _, n, ms)) = measured {
        summary.push(t!(
            "check.hot_rows.summary.between",
            when = crate::queries::short_when(&b.taken_at),
            n = n,
            waits = format::count(n),
            time = format::duration(ms),
            table = &table
        ));
        if hot.unchanged {
            summary.push(t!("check.hot_rows.summary.unchanged"));
        }
    }
    let (count, total, average) =
        (format::count(waits), total_wait(wait_ms), format::duration(per_run(wait_ms, waits)));
    summary.push(match indexes.as_slice() {
        [one] => t!(
            "check.hot_rows.summary.waited_on_index",
            index = &one.name,
            n = waits,
            waits = count,
            table = &table,
            total = total,
            average = average
        ),
        _ => t!(
            "check.hot_rows.summary.waited_on_indexes",
            indexes = format::list(&indexes.iter().map(|i| i.name.clone()).collect::<Vec<_>>()),
            n = waits,
            waits = count,
            table = &table,
            total = total,
            average = average
        ),
    });
    if escalations > 0.0 {
        summary.push(t!("check.hot_rows.summary.escalated", n = escalations, escalations = format::count(escalations)));
    }
    summary.push(t!("check.hot_rows.summary.causes"));
    if measured.is_some() {
        if !hot.in_window && !hot.unchanged {
            summary.push(t!("check.hot_rows.summary.beyond_window"));
        }
    } else {
        summary.push(t!("check.hot_rows.summary.no_earlier"));
    }
    f.summary = summary.join(" ");
    f.queries = query_refs(&hot.locked);
    f.severity = hot.severity;
    severity_score(&mut f);
    f.actionability = hot.actionability;
    f.impact.label = if hot.unchanged {
        t!("check.hot_rows.impact.unchanged")
    } else {
        t!("check.hot_rows.impact.waiting", time = total_wait(shown_ms))
    };
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::new(t!("check.hot_rows.metric.historical_waits"), waits, Unit::Count),
        Metric::new(t!("check.hot_rows.metric.historical_time"), wait_ms, Unit::Ms),
        Metric::new(t!("check.hot_rows.metric.historical_average"), per_run(wait_ms, waits), Unit::Ms),
    ];
    if let Some((_, h, n, ms)) = measured {
        f.metrics.push(Metric::new(t!("check.hot_rows.metric.waits_since"), n, Unit::Count));
        f.metrics.push(Metric::new(t!("check.hot_rows.metric.time_since"), ms, Unit::Ms));
        f.metrics.push(Metric::new(t!("check.hot_rows.metric.interval"), h / 24.0, Unit::Days));
    }
    if let Some(r) = hot.per_day {
        f.metrics.push(Metric::new(t!("check.hot_rows.metric.per_day"), r, Unit::Ms));
    }
    if escalations > 0.0 {
        f.metrics.push(Metric::said(super::ESCALATIONS_METRIC, escalations, Unit::Count));
    }
    f.fix = Some(if !f.is_actionable() {
        Fix { summary: t!("check.hot_rows.fix.no_change"), sql: None, cost: None }
    } else if hot.rcsi_off {
        Fix {
            summary: t!("check.hot_rows.fix.short_transactions_rcsi", table = &table),
            sql: Some("-- Readers stop waiting for writers; test first\nALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;".into()),
            cost: None,
        }
    } else {
        Fix { summary: t!("check.hot_rows.fix.short_transactions", table = &table), sql: None, cost: None }
    });
    f.verify = vec![t!("check.hot_rows.verify", table = &table)];
    f.caveats.push(t!("check.hot_rows.caveat.since_loaded"));
    if measured.is_some_and(|(_, h, _, _)| h < culprit_core::RATE_MIN_HOURS) {
        f.caveats.push(t!("check.hot_rows.caveat.under_an_hour"));
    }
    f
}
