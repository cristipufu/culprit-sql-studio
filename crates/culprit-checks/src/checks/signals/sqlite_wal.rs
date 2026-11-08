use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::CounterReading;
use culprit_core::{Actionability, t};

pub fn sqlite_wal_info() -> CheckInfo {
    CheckInfo {
        id: "sqlite-wal".into(),
        title: t!("check.sqlite_wal.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Sqlite],
        requires: vec![],
        description: t!("check.sqlite_wal.info.description"),
    }
}

check!(
    SqliteWalCheck: sqlite_wal_info,
    decide = large_wal,
    render = sqlite_wal_finding,
    case<'s> = LargeWal<'s>,
    evidence = &[Evidence::Observed(Observation::WalSize, &[culprit_core::source_id::WAL_FILE])],
);

/// A WAL file of 64 MB or more, or one that grew 16 MB or more since an earlier scan.
pub struct LargeWal<'s> {
    bytes: f64,
    /// The earlier comparable size, and how much the file changed since.
    before: Option<(&'s CounterReading, f64)>,
}

fn large_wal(s: &Snapshot) -> Vec<LargeWal<'_>> {
    let Some(w) = s.operations.sqlite.as_ref() else { return vec![] };
    let Some(bytes) = w.wal_bytes.filter(|b| b.is_finite() && *b >= 0.0) else { return vec![] };
    let before = w
        .before
        .as_ref()
        .filter(|b| parse_epoch_seconds(&b.at).zip(parse_epoch_seconds(&s.taken_at)).is_some_and(|(a, b)| a < b));
    let growth = before.map(|p| bytes - p.count);
    if bytes < 64.0 * 1024.0 * 1024.0 && growth.is_none_or(|g| g < 16.0 * 1024.0 * 1024.0) {
        return vec![];
    }
    vec![LargeWal { bytes, before: before.map(|p| (p, bytes - p.count)) }]
}

fn sqlite_wal_finding(s: &Snapshot, wal: LargeWal<'_>) -> Finding {
    let mut f = finding(
        "sqlite-wal",
        &s.server.database,
        Category::Maintenance,
        subject(SubjectKind::Database, &s.server.database),
    );
    f.title = t!("check.sqlite_wal.title", size = format::bytes(wal.bytes));
    f.summary = [
        t!("check.sqlite_wal.summary.read"),
        match wal.before {
            Some((p, growth)) if growth < 0.0 => {
                t!("check.sqlite_wal.summary.shrank", size = format::bytes(growth.abs()), at = &p.at)
            }
            Some((p, growth)) => t!("check.sqlite_wal.summary.grew", size = format::bytes(growth.abs()), at = &p.at),
            None => t!("check.sqlite_wal.summary.no_earlier"),
        },
        t!("check.sqlite_wal.summary.not_established"),
    ]
    .join(" ");
    f.severity = Severity::Low;
    f.actionability = Actionability::Information;
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(t!("check.sqlite_wal.impact"));
    f.metrics = vec![Metric::said(culprit_core::key!("check.sqlite_wal.metric.length"), wal.bytes, Unit::Bytes)];
    f.fix = Some(Fix { summary: t!("check.sqlite_wal.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.sqlite_wal.verify")];
    f
}
