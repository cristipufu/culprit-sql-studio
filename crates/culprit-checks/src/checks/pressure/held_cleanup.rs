//! The `held-cleanup` check: The transaction that holds this database's cleanup back: one with uncommitted changes or row versions here (SQL Server), the oldest snapshot, replication slot or prepared transaction behind VACUUM's horizon (PostgreSQL), or the oldest read view behind InnoDB's purge (MySQL).

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::OpenTransaction;

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
    held: &'s OpenTransaction,
    holding: Holding<'s>,
    /// Transactions started since its snapshot (PostgreSQL), or how far behind a slot holds the horizon.
    behind: f64,
    /// The program it comes from, when the server says.
    program: Option<&'s str>,
    severity: Severity,
    confidence: Confidence,
}

/// What the transaction holds, as its engine keeps it.
enum Holding<'s> {
    /// A SQL Server session's open transaction.
    Mssql {
        /// What this database's version store holds while it stays open.
        version_store: Option<f64>,
        /// It reads or makes row versions.
        snapshot_reader: bool,
        /// It changed data here and hasn't committed.
        wrote: bool,
    },
    /// A replication slot holding VACUUM's horizon (or, `catalog_only`, only system-catalog cleanup).
    PgSlot { catalog_only: bool, inactive: bool },
    /// A prepared transaction waiting to be committed or rolled back, and what prepared it.
    PgPrepared { by: Option<&'s str> },
    /// A session's open transaction, `idle` when the application left it open doing nothing.
    PgSession { idle: bool },
    /// An InnoDB transaction, with the whole server's undo history and the database it runs in when not this one.
    Mysql { history: Option<f64>, other_database: Option<&'s str> },
    /// A transaction holding the file's lock.
    Sqlite,
}

fn held_cleanups(snapshot: &Snapshot) -> Vec<HeldCleanup<'_>> {
    let Some(t) = pressure(snapshot).and_then(|p| p.oldest_transaction.as_ref()) else { return Vec::new() };
    let engine = snapshot.server.engine;
    // Collectors report only what holds this database's cleanup, and say what it is.
    let slot = t.holder == CleanupHolder::ReplicationSlot;
    let held_gb = t.held_bytes.unwrap_or(0.0) / 1024.0 / 1024.0 / 1024.0;
    let behind = t.xid_age.unwrap_or(0.0);
    // How long it has been open is what tells a forgotten transaction from a busy one; a slot has no start time, so
    // it is how far behind it holds the horizon.
    let old = if slot { behind >= SLOT_BEHIND } else { t.seconds >= 1800.0 };
    let big = engine == Engine::Mssql && held_gb >= 1.0 && t.seconds >= 300.0;
    if !old && !big {
        return Vec::new();
    }
    let holding = match (engine, t.holder) {
        (Engine::Yugabyte, _) => return Vec::new(),
        (Engine::Mssql, _) => Holding::Mssql {
            version_store: t.held_bytes.filter(|b| *b > 0.0),
            snapshot_reader: t.held_bytes.is_some_and(|b| b > 0.0) || t.state.as_deref() == Some("snapshot"),
            wrote: t.wrote == Some(true),
        },
        (Engine::Postgres, CleanupHolder::ReplicationSlot) => {
            Holding::PgSlot { catalog_only: t.catalog_only, inactive: t.state.as_deref() == Some("inactive slot") }
        }
        (Engine::Postgres, CleanupHolder::PreparedTransaction) => Holding::PgPrepared { by: t.program.as_deref() },
        (Engine::Postgres, CleanupHolder::Session) => {
            Holding::PgSession { idle: t.state.as_deref().is_some_and(|s| s.contains("idle")) }
        }
        (Engine::Mysql, _) => Holding::Mysql {
            history: signal(snapshot, "history-length").filter(|h| *h > 0.0),
            other_database: t.database.as_deref().filter(|d| !d.eq_ignore_ascii_case(&snapshot.server.database)),
        },
        (Engine::Sqlite, _) => Holding::Sqlite,
    };
    vec![HeldCleanup {
        held: t,
        holding,
        behind,
        program: t.program.as_deref().filter(|p| !p.is_empty()),
        severity: if t.seconds >= 3.0 * 3600.0 || held_gb >= 10.0 || (slot && behind >= SLOT_FAR_BEHIND) {
            Severity::High
        } else {
            Severity::Medium
        },
        confidence: if engine == Engine::Mysql { Confidence::Medium } else { Confidence::High },
    }]
}

fn held_cleanup_finding(snapshot: &Snapshot, h: HeldCleanup<'_>) -> Finding {
    let t = h.held;
    let slot = t.holder == CleanupHolder::ReplicationSlot;
    let mut f = finding(
        "held-cleanup",
        &t.session,
        Category::Maintenance,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let (session, open, behind) = (&t.session, format::duration(t.seconds * 1000.0), format::count(h.behind));
    f.title = match &h.holding {
        Holding::Mssql { version_store: Some(b), .. } => {
            t!("check.held_cleanup.title.version_store", session = session, open = &open, size = format::bytes(*b))
        }
        Holding::Mssql { wrote: true, .. } => {
            t!("check.held_cleanup.title.uncommitted", session = session, open = &open)
        }
        Holding::Mssql { .. } => t!("check.held_cleanup.title.open", session = session, open = &open),
        Holding::PgSlot { catalog_only: true, .. } => {
            t!("check.held_cleanup.title.slot_catalog", session = session, behind = &behind)
        }
        Holding::PgSlot { .. } => t!("check.held_cleanup.title.slot", session = session, behind = &behind),
        Holding::PgPrepared { .. } => t!("check.held_cleanup.title.prepared", session = session, open = &open),
        Holding::PgSession { idle: true } => {
            t!("check.held_cleanup.title.session_idle", session = session, open = &open)
        }
        Holding::PgSession { idle: false } => t!("check.held_cleanup.title.session", session = session, open = &open),
        Holding::Mysql { .. } => t!("check.held_cleanup.title.mysql", session = session, open = &open),
        Holding::Sqlite => t!("check.held_cleanup.title.sqlite", open = &open),
    };
    let program = h.program.map(|p| t!("check.held_cleanup.summary.program", program = p));
    let behind_note = (h.behind > 0.0).then(|| t!("check.held_cleanup.summary.behind", behind = &behind));
    let mut summary = Vec::new();
    match &h.holding {
        Holding::Mssql { snapshot_reader, wrote, .. } => {
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
        Holding::PgSlot { catalog_only: true, .. } => {
            summary.push(t!("check.held_cleanup.summary.slot_catalog", behind = &behind))
        }
        Holding::PgSlot { inactive, .. } => {
            summary.push(t!("check.held_cleanup.summary.slot"));
            if *inactive {
                summary.push(t!("check.held_cleanup.summary.slot_inactive"));
            }
        }
        Holding::PgPrepared { by } => {
            summary.push(t!("check.held_cleanup.summary.prepared"));
            summary.extend(behind_note);
            summary.extend(by.map(|by| t!("check.held_cleanup.summary.prepared_by", program = by)));
        }
        Holding::PgSession { idle } => {
            summary.push(t!("check.held_cleanup.summary.session"));
            summary.extend(behind_note);
            if *idle {
                summary.push(t!("check.held_cleanup.summary.idle"));
            }
            summary.extend(program);
        }
        Holding::Mysql { history, other_database } => {
            summary.push(t!("check.held_cleanup.summary.mysql"));
            summary.extend(
                history.map(|length| t!("check.held_cleanup.summary.undo_history", length = format::count(length))),
            );
            summary.extend(other_database.map(|d| t!("check.held_cleanup.summary.other_database", database = d)));
            summary.extend(program);
        }
        Holding::Sqlite => {
            summary.push(t!("check.held_cleanup.summary.sqlite"));
            summary.extend(program);
        }
    }
    f.summary = summary.join(" ");
    f.severity = h.severity;
    f.impact.score = base_score(f.severity);
    f.impact.label = if slot {
        t!("check.held_cleanup.impact.behind", behind = &behind)
    } else {
        t!("check.held_cleanup.impact.open", open = &open)
    };
    f.confidence = h.confidence;
    if !slot {
        f.metrics.push(Metric::new(t!("check.held_cleanup.metric.open"), t.seconds * 1000.0, Unit::Ms));
    }
    if let Some(b) = t.held_bytes.filter(|b| *b > 0.0) {
        f.metrics.push(Metric::new(t!("check.held_cleanup.metric.version_store"), b, Unit::Bytes));
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
            sql: Some(format!("ROLLBACK PREPARED '{}';", t.session.replace('\'', "''"))),
            cost: Some(t!("check.held_cleanup.fix_cost.prepared")),
        },
        CleanupHolder::Session => {
            let end = snapshot.server.engine.dialect().end_session(&t.session);
            Fix {
                summary: t!("check.held_cleanup.fix.session"),
                sql: Some(format!("-- Ends the session and rolls back its open transaction\n{end}")),
                cost: Some(t!("check.held_cleanup.fix_cost.session")),
            }
        }
    });
    f.verify = vec![if slot { t!("check.held_cleanup.verify.slot") } else { t!("check.held_cleanup.verify.open") }];
    f
}
