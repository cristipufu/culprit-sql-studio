//! The `xid-age` check: Transaction ID age approaching the point where PostgreSQL stops accepting writes to protect data.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::{OpenTransaction, TableStat, t};

pub fn xid_age_info() -> CheckInfo {
    CheckInfo {
        id: "xid-age".into(),
        title: t!("check.xid_age.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres],
        requires: vec![Capability::TableStats],
        description: t!("check.xid_age.info.description"),
    }
}

check!(
    XidAgeCheck: xid_age_info,
    decide = xid_ages,
    render = xid_age_finding,
    case<'s> = XidAge<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::Xid)],
);

pub(super) const WRAPAROUND: f64 = 2_000_000_000.0;

/// The database's oldest unfrozen transaction ID, old enough that freezing is falling behind.
pub struct XidAge<'s> {
    /// Its age: the database's, else its oldest table's.
    age: f64,
    /// autovacuum_freeze_max_age, when the age is past it: autovacuum should be freezing and isn't keeping up.
    past_freeze_max: Option<f64>,
    /// The cleanup holder this scan observed: VACUUM can't freeze row versions newer than its horizon.
    holder: Option<&'s OpenTransaction>,
    /// The table with the oldest transaction ID, to freeze first.
    oldest: Option<&'s TableStat>,
    severity: Severity,
    score: f64,
}

fn xid_ages(snapshot: &Snapshot) -> Vec<XidAge<'_>> {
    if snapshot.server.engine != Engine::Postgres {
        return Vec::new();
    }
    let db_age = snapshot.setting("datfrozenxid_age").and_then(|s| s.value.parse::<f64>().ok());
    let oldest = snapshot
        .tables
        .iter()
        .filter(|t| t.xid_age.is_some())
        .max_by(|a, b| a.xid_age.unwrap_or(0.0).total_cmp(&b.xid_age.unwrap_or(0.0)));
    let age = db_age.or(oldest.and_then(|t| t.xid_age)).unwrap_or(0.0);
    // Autovacuum starts freezing a table on its own once its age passes autovacuum_freeze_max_age (200M by default,
    // and often raised on big databases): an age a little past it is freezing in progress. Twice it, or 500M when the
    // setting is unknown or low, means freezing is falling behind; never later than 1.2B, with 2B the hard stop.
    let freeze_max = snapshot.setting("autovacuum_freeze_max_age").and_then(|s| s.value.trim().parse::<f64>().ok());
    let threshold = freeze_max.map_or(500_000_000.0, |m| (m * 2.0).max(500_000_000.0)).min(1_200_000_000.0);
    if age < threshold {
        return Vec::new();
    }
    let holder = snapshot
        .pressure
        .as_ref()
        .and_then(|p| p.oldest_transaction.as_ref())
        .filter(|h| !h.catalog_only && h.xid_age.is_some_and(|a| a > 0.0));
    vec![XidAge {
        age,
        past_freeze_max: freeze_max.filter(|m| age >= *m),
        holder,
        oldest,
        severity: if age >= 1_200_000_000.0 { Severity::High } else { Severity::Medium },
        score: if age >= 1_500_000_000.0 {
            95.0
        } else if age >= 1_200_000_000.0 {
            70.0
        } else {
            35.0
        },
    }]
}

fn xid_age_finding(snapshot: &Snapshot, xid: XidAge<'_>) -> Finding {
    let age = xid.age;
    let remaining = (WRAPAROUND - age).max(0.0);
    let mut f = finding(
        "xid-age",
        &snapshot.server.database,
        Category::Maintenance,
        subject(SubjectKind::Database, &snapshot.server.database),
    );
    f.title = t!("check.xid_age.title", share = format::percent(age / WRAPAROUND));
    let mut summary = vec![t!("check.xid_age.summary.age", age = format::count(age))];
    if let Some(m) = xid.past_freeze_max {
        summary.push(t!(
            "check.xid_age.summary.freeze_max",
            freeze_max = format::count(m),
            times = format!("{:.1}", age / m.max(1.0))
        ));
    }
    if let Some(h) = xid.holder {
        summary.push(t!("check.xid_age.summary.holder", holder = cleanup_holder_phrase(h)));
    }
    summary.push(t!("check.xid_age.summary.wraparound", remaining = format::count(remaining)));
    f.summary = summary.join(" ");
    f.severity = xid.severity;
    f.impact.score = xid.score;
    f.impact.label = t!("check.xid_age.impact", remaining = format::count(remaining));
    f.confidence = Confidence::High;
    f.metrics.push(Metric::new(t!("check.xid_age.metric.age"), age, Unit::Count));
    f.metrics.push(Metric::new(t!("check.xid_age.metric.remaining"), remaining, Unit::Count));
    if let Some(t) = xid.oldest {
        f.metrics.push(Metric::text(t!("check.xid_age.metric.oldest_table"), t.qualified()));
    }
    if let Some(h) = xid.holder {
        f.metrics.push(Metric::text(t!("check.xid_age.metric.holder"), cleanup_holder_phrase(h)));
    }
    f.fix = Some(Fix {
        summary: t!("check.xid_age.fix"),
        sql: Some(match xid.oldest {
            Some(t) => format!("VACUUM (FREEZE, VERBOSE) {};", qualified(Engine::Postgres, &t.schema, &t.name)),
            None => "VACUUM (FREEZE, VERBOSE);".into(),
        }),
        cost: Some(t!("check.xid_age.fix_cost")),
    });
    f
}
