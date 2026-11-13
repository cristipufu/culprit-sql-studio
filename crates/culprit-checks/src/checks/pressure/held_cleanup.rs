//! The `held-cleanup` check: The transaction that holds this database's cleanup back: one with uncommitted changes or row versions here (SQL Server), the oldest snapshot, replication slot or prepared transaction behind VACUUM's horizon (PostgreSQL), or the oldest read view behind InnoDB's purge (MySQL).

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::telemetry::CleanupHorizon;
use culprit_core::{CleanupEvidence, OpenTransaction};

pub fn held_cleanup_info() -> CheckInfo {
    CheckInfo {
        id: "held-cleanup".into(),
        title: t!("check.held_cleanup.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.held_cleanup.info.description"),
    }
}

check!(
    HeldCleanupCheck: held_cleanup_info,
    decide = held_cleanups,
    render = held_cleanup_finding,
    case<'s> = HeldCleanup<'s>,
    evidence = &[Evidence::Pressure { signal: None, timed_files: false, complete: false }],
);

/// The held-cleanup findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn held_cleanup(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&HeldCleanupCheck, snapshot)
}

/// Transactions behind at which a replication slot's hold on VACUUM is worth a finding, and High.
pub(super) const SLOT_BEHIND: f64 = 1_000_000.0;
const SLOT_FAR_BEHIND: f64 = 100_000_000.0;

/// What holds this database's cleanup back, long enough (or, on SQL Server, holding enough) to matter.
pub struct HeldCleanup<'s> {
    held: OpenTransaction,
    holding: Holding<'s>,
    /// Transactions started since its snapshot (PostgreSQL), or how far behind a slot holds the horizon.
    behind: f64,
    /// The program it comes from, when the server says.
    program: Option<&'s str>,
    severity: Severity,
    confidence: Confidence,
    retention: Option<NativeRetention>,
}
struct NativeRetention {
    key: String,
    horizon: CleanupHorizon,
    complete: bool,
    tied: u32,
}

/// What the transaction holds, as its engine keeps it.
enum Holding<'s> {
    /// A SQL Server session's open transaction.
    Versioned {
        /// What this database's version store holds while it stays open.
        version_store: Option<f64>,
        /// It reads or makes row versions.
        snapshot_reader: bool,
        /// It changed data here and hasn't committed.
        wrote: bool,
    },
    /// A replication slot holding VACUUM's horizon (or, `catalog_only`, only system-catalog cleanup).
    Slot { catalog_only: bool, inactive: bool },
    /// A prepared transaction waiting to be committed or rolled back, and what prepared it.
    Prepared { by: Option<&'s str> },
    /// A session's open transaction, `idle` when the application left it open doing nothing.
    Snapshot { idle: bool },
    /// An InnoDB transaction, with the whole server's undo history and the database it runs in when not this one.
    SharedUndo { history: Option<f64>, other_database: Option<String> },
    /// A transaction holding the file's lock.
    FileLock,
}

fn held_cleanups(snapshot: &Snapshot) -> Vec<HeldCleanup<'_>> {
    let Some(pressure) = pressure(snapshot) else { return Vec::new() };
    if snapshot.server.engine.spec().cleanup_evidence() == CleanupEvidence::TransactionHorizons {
        let Some(retention) = pressure.retention.as_ref().filter(|retention| retention.is_valid()) else {
            return Vec::new();
        };
        let mut cases = Vec::new();
        for horizon in [CleanupHorizon::Data, CleanupHorizon::Catalog] {
            let complete = retention.is_horizon_complete(horizon);
            let oldest = retention.oldest_observed(horizon);
            let tied = oldest.len() as u32;
            for observation in oldest {
                if observation.holder != CleanupHolder::ReplicationSlot && observation.age_seconds.is_none() {
                    continue;
                }
                // A data horizon also protects catalogs; the same observation and distance needs one finding.
                if horizon == CleanupHorizon::Catalog
                    && observation.transactions_behind(CleanupHorizon::Data)
                        == observation.transactions_behind(CleanupHorizon::Catalog)
                {
                    continue;
                }
                let program = observation
                    .client
                    .as_ref()
                    .and_then(|client| client.application.as_deref().or(client.user.as_deref()));
                let Ok(key) = observation.key() else { continue };
                let native = NativeRetention { key, horizon, complete, tied };
                if let Some(case) = decide_holder(snapshot, observation.display(horizon), program, Some(native)) {
                    cases.push(case);
                }
            }
        }
        return cases;
    }
    pressure
        .oldest_transaction
        .as_ref()
        .and_then(|transaction| decide_holder(snapshot, transaction.clone(), transaction.program.as_deref(), None))
        .into_iter()
        .collect()
}

fn decide_holder<'s>(
    snapshot: &'s Snapshot,
    t: OpenTransaction,
    program: Option<&'s str>,
    retention: Option<NativeRetention>,
) -> Option<HeldCleanup<'s>> {
    let semantics = snapshot.server.engine.spec().cleanup_evidence();
    // Collectors report only what holds this database's cleanup, and say what it is.
    let slot = t.holder == CleanupHolder::ReplicationSlot;
    let held_gb = t.held_bytes.unwrap_or(0.0) / 1024.0 / 1024.0 / 1024.0;
    let behind = t.xid_age.unwrap_or(0.0);
    // How long it has been open is what tells a forgotten transaction from a busy one; a slot has no start time, so
    // it is how far behind it holds the horizon.
    let old = if slot { behind >= SLOT_BEHIND } else { t.seconds >= 1800.0 };
    let big = semantics == CleanupEvidence::LoggedAndVersionedTransactions && held_gb >= 1.0 && t.seconds >= 300.0;
    if !old && !big {
        return None;
    }
    let holding = match (semantics, t.holder) {
        (CleanupEvidence::Unavailable, _) => return None,
        (CleanupEvidence::LoggedAndVersionedTransactions, _) => Holding::Versioned {
            version_store: t.held_bytes.filter(|b| *b > 0.0),
            snapshot_reader: t.held_bytes.is_some_and(|b| b > 0.0) || t.activity == TransactionActivity::Snapshot,
            wrote: t.wrote == Some(true),
        },
        (CleanupEvidence::TransactionHorizons, CleanupHolder::ReplicationSlot) => {
            Holding::Slot { catalog_only: t.catalog_only, inactive: t.activity == TransactionActivity::SlotInactive }
        }
        (CleanupEvidence::TransactionHorizons, CleanupHolder::PreparedTransaction) => Holding::Prepared { by: program },
        (CleanupEvidence::TransactionHorizons, CleanupHolder::Session) => {
            Holding::Snapshot { idle: t.activity == TransactionActivity::IdleInTransaction }
        }
        (CleanupEvidence::SharedUndoReadViews, _) => Holding::SharedUndo {
            history: signal(snapshot, "history-length").filter(|h| *h > 0.0),
            other_database: t.database.as_ref().filter(|d| !d.eq_ignore_ascii_case(&snapshot.server.database)).cloned(),
        },
        (CleanupEvidence::FileLock, _) => Holding::FileLock,
    };
    let severity = if t.seconds >= 3.0 * 3600.0 || held_gb >= 10.0 || (slot && behind >= SLOT_FAR_BEHIND) {
        Severity::High
    } else {
        Severity::Medium
    };
    Some(HeldCleanup {
        held: t,
        holding,
        behind,
        program: program.filter(|p| !p.is_empty()),
        severity,
        confidence: if semantics == CleanupEvidence::SharedUndoReadViews
            || retention.as_ref().is_some_and(|native| !native.complete)
        {
            Confidence::Medium
        } else {
            Confidence::High
        },
        retention,
    })
}

fn held_cleanup_finding(snapshot: &Snapshot, h: HeldCleanup<'_>) -> Finding {
    let t = &h.held;
    let slot = t.holder == CleanupHolder::ReplicationSlot;
    let identity = h.retention.as_ref().map(|native| {
        format!("{}:{}", native.key, if native.horizon == CleanupHorizon::Data { "data" } else { "catalog" })
    });
    let mut f = finding(
        "held-cleanup",
        identity.as_deref().unwrap_or(&t.session),
        Category::Maintenance,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    f.facts.cleanup_holder = Some(t.session.clone());
    if let Some(native) = &h.retention {
        f.facts.cleanup_horizon = Some(native.horizon);
        f.facts.cleanup_retention_key = Some(native.key.clone());
        f.facts.cleanup_visibility_complete = Some(native.complete);
        f.facts.cleanup_tied_holders = Some(native.tied);
    }
    let (session, open, behind) = (&t.session, format::duration(t.seconds * 1000.0), format::count(h.behind));
    f.title = match &h.holding {
        Holding::Versioned { version_store: Some(b), .. } => {
            t!("check.held_cleanup.title.version_store", session = session, open = &open, size = format::bytes(*b))
        }
        Holding::Versioned { wrote: true, .. } => {
            t!("check.held_cleanup.title.uncommitted", session = session, open = &open)
        }
        Holding::Versioned { .. } => t!("check.held_cleanup.title.open", session = session, open = &open),
        Holding::Slot { catalog_only: true, .. } => {
            t!("check.held_cleanup.title.slot_catalog", session = session, behind = &behind)
        }
        Holding::Slot { .. } => t!("check.held_cleanup.title.slot", session = session, behind = &behind),
        Holding::Prepared { .. } => t!("check.held_cleanup.title.prepared", session = session, open = &open),
        Holding::Snapshot { idle: true } => {
            t!("check.held_cleanup.title.session_idle", session = session, open = &open)
        }
        Holding::Snapshot { idle: false } => t!("check.held_cleanup.title.session", session = session, open = &open),
        Holding::SharedUndo { .. } => t!("check.held_cleanup.title.mysql", session = session, open = &open),
        Holding::FileLock => t!("check.held_cleanup.title.sqlite", open = &open),
    };
    let program = h.program.map(|p| t!("check.held_cleanup.summary.program", program = p));
    let behind_note = (h.behind > 0.0).then(|| t!("check.held_cleanup.summary.behind", behind = &behind));
    let mut summary = Vec::new();
    match &h.holding {
        Holding::Versioned { snapshot_reader, wrote, .. } => {
            if *snapshot_reader {
                summary.push(t!("check.held_cleanup.summary.version_store"));
            }
            if *wrote {
                summary.push(t!("check.held_cleanup.summary.uncommitted"));
            }
            if summary.is_empty() {
                summary.push(t!("check.held_cleanup.summary.open"));
            }
            summary.extend(program);
            summary.push(t!("check.held_cleanup.summary.short"));
        }
        Holding::Slot { catalog_only: true, .. } => {
            summary.push(t!("check.held_cleanup.summary.slot_catalog", behind = &behind))
        }
        Holding::Slot { inactive, .. } => {
            summary.push(t!("check.held_cleanup.summary.slot"));
            if *inactive {
                summary.push(t!("check.held_cleanup.summary.slot_inactive"));
            }
        }
        Holding::Prepared { by } => {
            summary.push(t!("check.held_cleanup.summary.prepared"));
            summary.extend(behind_note);
            summary.extend(by.map(|by| t!("check.held_cleanup.summary.prepared_by", program = by)));
        }
        Holding::Snapshot { idle } => {
            summary.push(t!("check.held_cleanup.summary.session"));
            summary.extend(behind_note);
            if *idle {
                summary.push(t!("check.held_cleanup.summary.idle"));
            }
            summary.extend(program);
        }
        Holding::SharedUndo { history, other_database } => {
            summary.push(t!("check.held_cleanup.summary.mysql"));
            summary.extend(
                history.map(|length| t!("check.held_cleanup.summary.undo_history", length = format::count(length))),
            );
            summary.extend(
                other_database.as_deref().map(|d| t!("check.held_cleanup.summary.other_database", database = d)),
            );
            summary.extend(program);
        }
        Holding::FileLock => {
            summary.push(t!("check.held_cleanup.summary.sqlite"));
            summary.extend(program);
        }
    }
    if let Some(native) = &h.retention {
        if !native.complete {
            summary.push(t!("check.held_cleanup.summary.incomplete"));
        }
        if native.tied > 1 {
            summary.push(t!("check.held_cleanup.summary.tied", count = format::count(native.tied as f64)));
        }
    }
    f.summary = summary.join(" ");
    f.severity = h.severity;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(if slot {
        t!("check.held_cleanup.impact.behind", behind = &behind)
    } else {
        t!("check.held_cleanup.impact.open", open = &open)
    });
    f.confidence = h.confidence;
    if !slot {
        f.metrics.push(Metric::said(
            culprit_core::key!("check.held_cleanup.metric.open"),
            t.seconds * 1000.0,
            Unit::Ms,
        ));
    }
    if let Some(b) = t.held_bytes.filter(|b| *b > 0.0) {
        f.metrics.push(Metric::said(culprit_core::key!("check.held_cleanup.metric.version_store"), b, Unit::Bytes));
    }
    if h.behind > 0.0 {
        f.metrics.push(Metric::said(key!("check.held_cleanup.metric.behind"), h.behind, Unit::Count));
    }
    f.fix = Some(match t.holder {
        CleanupHolder::ReplicationSlot => Fix {
            summary: t!("check.held_cleanup.fix.slot"),
            sql: None,
            cost: Some(t!("check.held_cleanup.fix_cost.slot")),
        },
        CleanupHolder::PreparedTransaction => Fix {
            summary: t!("check.held_cleanup.fix.prepared"),
            sql: h.retention.is_none().then(|| format!("ROLLBACK PREPARED '{}';", t.session.replace('\'', "''"))),
            cost: Some(t!("check.held_cleanup.fix_cost.prepared")),
        },
        CleanupHolder::Session => {
            let end = snapshot.server.engine.dialect().end_session(&t.session);
            Fix {
                summary: t!("check.held_cleanup.fix.session"),
                // Native retention observations name a generation, not a safe PID-only termination target.
                sql: h
                    .retention
                    .is_none()
                    .then(|| format!("-- Ends the session and rolls back its open transaction\n{end}")),
                cost: Some(t!("check.held_cleanup.fix_cost.session")),
            }
        }
    });
    f.verify = vec![if slot { t!("check.held_cleanup.verify.slot") } else { t!("check.held_cleanup.verify.open") }];
    f
}
