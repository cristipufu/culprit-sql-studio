//! The `dead-rows` check: Tables carrying many dead tuples, which bloat storage and slow scans.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::{OpenTransaction, TableStat, t};

pub fn dead_rows_info() -> CheckInfo {
    CheckInfo {
        id: "dead-rows".into(),
        title: t!("check.dead_rows.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres],
        requires: vec![Capability::TableStats],
        description: t!("check.dead_rows.info.description"),
    }
}

check!(
    DeadRowsCheck: dead_rows_info,
    decide = dead_rows_cases,
    render = dead_rows_finding,
    case<'s> = DeadRows<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::DeadRows), Evidence::Settings],
);

/// The dead-rows findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn dead_rows(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&DeadRowsCheck, snapshot)
}

/// When autovacuum vacuums `t`: its own storage parameters, else the server's settings, else PostgreSQL's defaults
/// (50 rows + 20% of the table). `None` when autovacuum is off for it (or for the server).
pub(super) fn vacuum_trigger(snapshot: &Snapshot, t: &culprit_core::TableStat, live: f64) -> Option<f64> {
    autovacuum_trigger(snapshot, t, live, "vacuum")
}

/// An autovacuum setting as it applies to `t`: its own storage option, else the server's.
pub(super) fn autovacuum_setting(snapshot: &Snapshot, t: &culprit_core::TableStat, name: &str) -> Option<f64> {
    t.storage
        .as_ref()
        .and_then(|s| s.option(name))
        .or_else(|| snapshot.setting(name).and_then(|s| s.value.trim().parse::<f64>().ok()))
}

/// What the dead rows of a table establish.
pub enum DeadRows<'s> {
    /// Dead rows past twice the table's autovacuum trigger, or piling up with autovacuum off for it.
    Backlog(Backlog<'s>),
    /// A multi-million-row table whose scale factor makes autovacuum wait for a large pile of dead rows.
    ScaleFactor(ScaleFactor<'s>),
    /// Autovacuum switched off in the table's storage parameters.
    Disabled(Disabled<'s>),
}

/// A table with more dead rows than autovacuum lets pile up, and why they are there.
pub struct Backlog<'s> {
    table: &'s TableStat,
    live: f64,
    dead: f64,
    /// Dead rows' share of all its rows.
    ratio: f64,
    /// The dead rows at which autovacuum visits it; `None` with autovacuum off for it.
    trigger: Option<f64>,
    cause: Cause<'s>,
    last_vacuum: LastVacuum<'s>,
    /// The storage options the fix sets, when it vacuums: a smaller scale factor, autovacuum back on, or both.
    options: Option<&'static str>,
    severity: Severity,
}

/// What the scan establishes about why the dead rows are there, which also decides the fix and how to verify it.
enum Cause<'s> {
    /// A cleanup holder was observed: it can keep some of them, how many isn't known.
    Held(&'s OpenTransaction),
    /// Vacuum ran within the last hour: they may have died since, or an older snapshot may keep them.
    RecentVacuum,
    /// Autovacuum should have visited at this many dead rows, and is behind.
    Behind(f64),
    /// Autovacuum is off for the table.
    Off,
}

/// When the table was last vacuumed.
enum LastVacuum<'s> {
    /// This many days before the scan.
    DaysAgo(f64),
    /// On this date, which doesn't read as before the scan.
    On(&'s str),
    Never,
}

/// A large, busy table whose scale factor makes autovacuum wait for many dead rows before it starts.
pub struct ScaleFactor<'s> {
    table: &'s TableStat,
    live: f64,
    dead: f64,
    trigger: f64,
    factor: f64,
}

/// A table whose storage parameters switch autovacuum off.
pub struct Disabled<'s> {
    table: &'s TableStat,
    /// The option as the table's reloptions say it.
    option: &'s str,
    /// It is the table's TOAST table that autovacuum skips.
    toast: bool,
    live: f64,
    dead: f64,
    severity: Severity,
}

fn dead_rows_cases(snapshot: &Snapshot) -> Vec<DeadRows<'_>> {
    if snapshot.server.engine != Engine::Postgres {
        return Vec::new();
    }
    let backlogs: Vec<Backlog<'_>> = snapshot.tables.iter().filter_map(|t| backlog(snapshot, t)).collect();
    let behind: Vec<String> = backlogs.iter().map(|b| b.table.qualified()).collect();
    let mut out: Vec<DeadRows<'_>> = backlogs.into_iter().map(DeadRows::Backlog).collect();
    out.extend(large_table_autovacuum_scale(snapshot, &behind).into_iter().map(DeadRows::ScaleFactor));
    out.extend(disabled_autovacuum(snapshot, &behind).into_iter().map(DeadRows::Disabled));
    out
}

fn backlog<'s>(snapshot: &'s Snapshot, t: &'s TableStat) -> Option<Backlog<'s>> {
    let live = t.rows?;
    let dead = t.dead_rows?;
    // A busy table reaches its trigger (by default 20% of it dead) between every two autovacuum runs: that is
    // autovacuum's rhythm. Past twice the trigger it is behind; with autovacuum off for the table, any real pile of
    // dead rows stays.
    let trigger = vacuum_trigger(snapshot, t, live);
    let limit = trigger.map_or(live * 0.2, |tr| tr * DEAD_MARGIN);
    if dead < 10_000.0 || dead < limit {
        return None;
    }
    let ratio = dead / (live + dead).max(1.0);
    let last = t.last_vacuum.as_deref().and_then(|d| days_between(d, &snapshot.taken_at)).filter(|d| *d >= 0.0);
    // A recent vacuum and a dead-row estimate do not prove a blocked horizon: writes may have followed it.
    let recent_vacuum = last.is_some_and(|d| d * 24.0 < 1.0);
    let holder = snapshot
        .pressure
        .as_ref()
        .and_then(|p| p.oldest_transaction.as_ref())
        .filter(|h| !h.catalog_only && h.xid_age.is_some_and(|age| age > 0.0));
    let cause = match (trigger, holder) {
        (Some(_), Some(h)) => Cause::Held(h),
        (Some(_), None) if recent_vacuum => Cause::RecentVacuum,
        (Some(tr), None) => Cause::Behind(tr),
        (None, _) => Cause::Off,
    };
    // Sooner, never later: a table already set below 0.02 keeps its own factor.
    let factor = autovacuum_setting(snapshot, t, "autovacuum_vacuum_scale_factor").unwrap_or(0.2);
    let options = match (factor > 0.02, trigger.is_none()) {
        (true, true) => Some("autovacuum_vacuum_scale_factor = 0.02, autovacuum_enabled = true"),
        (true, false) => Some("autovacuum_vacuum_scale_factor = 0.02"),
        (false, true) => Some("autovacuum_enabled = true"),
        (false, false) => None,
    };
    Some(Backlog {
        table: t,
        live,
        dead,
        ratio,
        trigger,
        cause,
        last_vacuum: match (t.last_vacuum.as_deref(), last) {
            (Some(_), Some(days)) => LastVacuum::DaysAgo(days),
            (Some(d), None) => LastVacuum::On(d),
            (None, _) => LastVacuum::Never,
        },
        options,
        severity: if t.storage.as_ref().is_some_and(|s| s.autovacuum_off()) {
            Severity::High
        } else if ratio >= 0.5 && dead >= 1_000_000.0 {
            Severity::Medium
        } else {
            Severity::Low
        },
    })
}

/// Multi-million-row tables (`>= 5M` live rows) with active row churn (`>= 100k` dead rows or `>= 250k` updates/deletes)
/// that still use a large `autovacuum_vacuum_scale_factor` (`>= 0.1`), so autovacuum waits for a million or more dead
/// rows before starting. Tables already `behind` have a finding of their own.
fn large_table_autovacuum_scale<'s>(snapshot: &'s Snapshot, behind: &[String]) -> Vec<ScaleFactor<'s>> {
    let server_scale =
        snapshot.setting("autovacuum_vacuum_scale_factor").and_then(|s| s.value.trim().parse::<f64>().ok());
    snapshot
        .tables
        .iter()
        .filter_map(|t| {
            if t.partition_of.is_some() {
                return None;
            }
            let live = t.rows.filter(|r| *r >= 5_000_000.0)?;
            let dead = t.dead_rows.unwrap_or(0.0);
            let churn = t.updates.unwrap_or(0.0) + t.deletes.unwrap_or(0.0);
            if dead < 100_000.0 && churn < 250_000.0 {
                return None;
            }
            let table_name = t.qualified();
            if behind.contains(&table_name) {
                return None;
            }
            // Tables >= 10M rows without a per-table scale factor on a server with autovacuum_vacuum_scale_factor >= 0.2
            // and >= 1% churn are already covered by `config:autovacuum-scale`.
            let inherits_server_scale = t
                .storage
                .as_ref()
                .is_none_or(|s| s.option("autovacuum_vacuum_scale_factor").is_none() && !s.autovacuum_off());
            if live >= 10_000_000.0
                && inherits_server_scale
                && server_scale.is_some_and(|v| v >= 0.2)
                && ((t.updates.is_none() && t.deletes.is_none()) || churn >= live * 0.01)
            {
                return None;
            }
            let trigger = vacuum_trigger(snapshot, t, live)?;
            let factor = autovacuum_setting(snapshot, t, "autovacuum_vacuum_scale_factor").unwrap_or(0.2);
            if factor < 0.1 || trigger < 500_000.0 {
                return None;
            }
            Some(ScaleFactor { table: t, live, dead, trigger, factor })
        })
        .collect()
}

/// PostgreSQL tables where per-table `autovacuum_enabled` (or `toast.autovacuum_enabled`) was explicitly set to
/// `false` / `off` in `reloptions`.
fn disabled_autovacuum<'s>(snapshot: &'s Snapshot, behind: &[String]) -> Vec<Disabled<'s>> {
    snapshot
        .tables
        .iter()
        .filter_map(|t| {
            let table_name = t.qualified();
            // A table whose main heap already has a backlog finding includes `autovacuum_enabled = true` in that
            // finding's fix; a separate finding only when it hasn't one or `toast.autovacuum_enabled` is off.
            let has_main_dead_rows = behind.iter().any(|b| b.eq_ignore_ascii_case(&table_name));
            let options = &t.storage.as_ref()?.options;
            let option = options.iter().find(|o| {
                let Some((k, v)) = o.split_once('=') else { return false };
                let k = k.trim();
                let v = v.trim();
                let is_main = k.eq_ignore_ascii_case("autovacuum_enabled");
                let is_toast = k.eq_ignore_ascii_case("toast.autovacuum_enabled");
                ((is_main && !has_main_dead_rows) || is_toast)
                    && matches!(v.to_ascii_lowercase().as_str(), "false" | "off" | "no" | "0")
            })?;
            let live = t.rows.unwrap_or(0.0);
            let dead = t.dead_rows.unwrap_or(0.0);
            Some(Disabled {
                table: t,
                option,
                toast: option.trim().to_ascii_lowercase().starts_with("toast."),
                live,
                dead,
                severity: if dead >= 10_000.0 || live >= 100_000.0 || t.xid_age.unwrap_or(0.0) >= 50_000_000.0 {
                    Severity::High
                } else {
                    Severity::Medium
                },
            })
        })
        .collect()
}

fn dead_rows_finding(_: &Snapshot, case: DeadRows<'_>) -> Finding {
    match case {
        DeadRows::Backlog(b) => backlog_finding(b),
        DeadRows::ScaleFactor(s) => scale_factor_finding(s),
        DeadRows::Disabled(d) => disabled_finding(d),
    }
}

fn backlog_finding(b: Backlog<'_>) -> Finding {
    let t = b.table;
    let table_name = t.qualified();
    let mut f = finding("dead-rows", &table_name, Category::Maintenance, subject(SubjectKind::Table, &table_name));
    f.title = t!("check.dead_rows.title", share = format::percent(b.ratio), table = &t.name);
    let mut summary = vec![t!(
        "check.dead_rows.summary.dead",
        table = &table_name,
        dead = format::count(b.dead),
        live = format::count(b.live)
    )];
    summary.push(match b.cause {
        Cause::Held(h) => t!("check.dead_rows.summary.held", holder = cleanup_holder_phrase(h)),
        Cause::RecentVacuum => t!("check.dead_rows.summary.recent_vacuum"),
        Cause::Behind(tr) => t!(
            "check.dead_rows.summary.behind",
            trigger = format::count(tr),
            times = format!("{:.1}", b.dead / tr.max(1.0))
        ),
        Cause::Off => t!("check.dead_rows.summary.off"),
    });
    summary.push(match b.last_vacuum {
        LastVacuum::DaysAgo(days) => {
            t!("check.dead_rows.summary.vacuumed_ago", ago = format::duration(days * 86_400_000.0))
        }
        LastVacuum::On(date) => t!("check.dead_rows.summary.vacuumed_on", date = short_date(date)),
        LastVacuum::Never => t!("check.dead_rows.summary.never_vacuumed"),
    });
    f.summary = summary.join(" ");
    f.severity = b.severity;
    f.impact.score = base_score(f.severity) + b.ratio * 10.0;
    f.impact.set_label(t!("check.dead_rows.impact", dead = format::count(b.dead)));
    f.confidence = Confidence::High;
    f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.dead"), b.dead, Unit::Rows));
    f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.live"), b.live, Unit::Rows));
    f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.dead_share"), b.ratio, Unit::Ratio));
    if let Some(tr) = b.trigger {
        f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.trigger"), tr, Unit::Rows));
    }
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    f.fix = Some(match b.cause {
        Cause::Held(_) => Fix { summary: t!("check.dead_rows.fix.held"), sql: None, cost: None },
        Cause::RecentVacuum => Fix { summary: t!("check.dead_rows.fix.recent_vacuum"), sql: None, cost: None },
        Cause::Behind(_) | Cause::Off => Fix {
            summary: t!("check.dead_rows.fix.vacuum"),
            sql: Some(match b.options {
                Some(o) => format!("VACUUM (ANALYZE) {target};\nALTER TABLE {target} SET ({o});"),
                None => format!("VACUUM (ANALYZE) {target};"),
            }),
            cost: Some(t!("check.dead_rows.fix_cost.vacuum")),
        },
    });
    f.verify = vec![match b.cause {
        Cause::Held(_) => t!("check.dead_rows.verify.held"),
        Cause::RecentVacuum => t!("check.dead_rows.verify.recent_vacuum"),
        Cause::Behind(_) | Cause::Off => t!("check.dead_rows.verify.vacuum"),
    }];
    f
}

fn scale_factor_finding(s: ScaleFactor<'_>) -> Finding {
    let t = s.table;
    let table_name = t.qualified();
    let mut f = finding(
        "dead-rows",
        &format!("scale-factor:{table_name}"),
        Category::Maintenance,
        subject(SubjectKind::Table, &table_name),
    );
    f.actionability = culprit_core::Actionability::OptionalImprovement;
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.set_label(t!("check.dead_rows.scale_factor.impact", trigger = format::count(s.trigger)));
    f.confidence = Confidence::High;
    f.title = t!(
        "check.dead_rows.scale_factor.title",
        table = &t.name,
        live = format::count(s.live),
        trigger = format::count(s.trigger)
    );
    f.summary = t!(
        "check.dead_rows.scale_factor.summary",
        table = &table_name,
        live = format::count(s.live),
        dead = format::count(s.dead),
        factor = format::percent(s.factor),
        trigger = format::count(s.trigger)
    );
    f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.live"), s.live, Unit::Rows));
    if s.dead > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.dead"), s.dead, Unit::Rows));
    }
    f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.trigger"), s.trigger, Unit::Rows));
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    f.fix = Some(Fix {
        summary: t!("check.dead_rows.scale_factor.fix", table = &table_name),
        sql: Some(format!(
            "ALTER TABLE {target} SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.005);"
        )),
        cost: Some(t!("check.dead_rows.scale_factor.fix_cost")),
    });
    f.verify = vec![t!("check.dead_rows.scale_factor.verify", table = &table_name)];
    f
}

fn disabled_finding(d: Disabled<'_>) -> Finding {
    let t = d.table;
    let table_name = t.qualified();
    let param = if d.toast { "toast.autovacuum_enabled" } else { "autovacuum_enabled" };
    let mut f = finding(
        "dead-rows",
        &format!("autovacuum-disabled:{table_name}"),
        Category::Maintenance,
        subject(SubjectKind::Table, &table_name),
    );
    f.actionability = culprit_core::Actionability::NeedsAttention;
    f.confidence = Confidence::High;
    f.severity = d.severity;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(t!("check.dead_rows.disabled.impact", param = param));
    f.title = t!("check.dead_rows.disabled.title", table = &t.name, param = param);
    f.summary = if d.toast {
        t!("check.dead_rows.disabled.summary_toast", table = &table_name, option = d.option, param = param)
    } else {
        t!("check.dead_rows.disabled.summary", table = &table_name, option = d.option, param = param, name = &t.name)
    };
    if d.live > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.live"), d.live, Unit::Rows));
    }
    if d.dead > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("check.dead_rows.metric.dead"), d.dead, Unit::Rows));
    }
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    f.fix = Some(Fix {
        summary: t!("check.dead_rows.disabled.fix", table = &table_name, param = param),
        sql: Some(format!("ALTER TABLE {target} RESET ({param});")),
        cost: Some(t!("check.dead_rows.disabled.fix_cost")),
    });
    f.verify = vec![t!("check.dead_rows.disabled.verify", param = param, option = d.option, table = &table_name)];
    f
}
