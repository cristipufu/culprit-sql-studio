//! Replication states and native lag, with WAL retained by slots without a current consumer.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn replica_lag_info() -> CheckInfo {
    CheckInfo {
        id: "replica-lag".into(),
        title: t!("check.replica_lag.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.replica_lag.info.description"),
    }
}

check!(
    ReplicaLagCheck: replica_lag_info,
    decide = lagging,
    render = replica_lag_finding,
    case<'s> = Lagging<'s>,
    evidence = &[Evidence::Pressure { signal: None, timed_files: false, complete: false }],
);

/// How a replica, a consumer or this server stands, as replica-lag reads it.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ReplicaStatus {
    /// Replication failed or can't connect: the copy stopped with an error, or a standby meant to stream has no
    /// receiver.
    Stopped,
    /// Both threads report No without a recorded error. The source does not establish why or expected intent.
    ThreadsStopped,
    /// Behind by this many seconds beyond any configured delay (or by bytes, when that is all the engine says).
    Behind(Lag),
    Fine,
}

/// How far behind a replica is, as the engine measures it.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Lag {
    /// Seconds beyond any configured delay.
    Seconds(f64),
    /// Bytes not yet applied, when the engine gives no time.
    Bytes(f64),
}

pub(super) fn replica_status(r: &Replica) -> ReplicaStatus {
    // Group Replication's own channels idle by design; the group reports its members elsewhere.
    if r.name.to_ascii_lowercase().starts_with("group_replication_") {
        return ReplicaStatus::Fine;
    }
    match r.condition {
        ReplicaCondition::ThreadsStopped => return ReplicaStatus::ThreadsStopped,
        c if c.is_stopped() => return ReplicaStatus::Stopped,
        _ => {}
    }
    let seconds = r.lag_seconds;
    // A configured delay keeps WAL waiting on purpose: only the seconds beyond it say whether it is behind.
    let bytes = r.lag_bytes.filter(|_| r.delay_seconds.is_none() && seconds.is_none_or(|s| s > 0.0));
    if !(seconds.is_some_and(|s| s >= 30.0) || bytes.is_some_and(|b| b >= GB)) {
        return ReplicaStatus::Fine;
    }
    // The lag it is behind by: the seconds the engine measures, else the bytes it hasn't applied.
    match (seconds, r.lag_bytes) {
        (Some(s), _) => ReplicaStatus::Behind(Lag::Seconds(s)),
        (None, Some(b)) => ReplicaStatus::Behind(Lag::Bytes(b)),
        (None, None) => ReplicaStatus::Fine,
    }
}

/// How far behind, as a sentence says it: "2 min", "1.5 GB of log".
fn lag_words(lag: Lag) -> String {
    match lag {
        Lag::Seconds(s) => format::duration(s * 1000.0),
        Lag::Bytes(b) => t!("check.replica_lag.lag_bytes", bytes = format::bytes(b)),
    }
}

/// Native replication states and slot retention points; inactivity alone does not establish abandonment.
pub enum Lagging<'s> {
    Replica { replica: &'s Replica, status: ReplicaStatus },
    Slot(InactiveSlot<'s>),
    LostSlot(&'s Replica),
}

/// An inactive replication slot that keeps a gigabyte or more of WAL, and what can cap it.
pub struct InactiveSlot<'s> {
    slot: &'s Replica,
    kept: f64,
    logical: bool,
    /// max_slot_wal_keep_size caps what it keeps (13+).
    capped: bool,
    /// The scan read whether something caps it.
    cap_known: bool,
    /// The server can cap what slots keep: PostgreSQL 13 and later.
    can_cap: bool,
    severity: Severity,
    actionability: culprit_core::Actionability,
}

fn lagging(snapshot: &Snapshot) -> Vec<Lagging<'_>> {
    let Some(p) = snapshot.pressure.as_ref() else { return Vec::new() };
    p.replicas
        .iter()
        .filter_map(|r| match r.role {
            ReplicaRole::Slot if r.wal_status.as_deref() == Some("lost") => Some(Lagging::LostSlot(r)),
            ReplicaRole::Slot => inactive_slot(snapshot, r).map(Lagging::Slot),
            _ => stopped_or_behind(snapshot, r).map(|status| Lagging::Replica { replica: r, status }),
        })
        .collect()
}

/// How a replica, a logical consumer or this server stands, when it is stopped or behind.
fn stopped_or_behind(snapshot: &Snapshot, r: &Replica) -> Option<ReplicaStatus> {
    // A logical consumer of another database on this server is that database's lag (its slot, kept server-wide, is
    // still reported as a slot).
    if r.is_logical() && r.database.as_deref().is_some_and(|d| !d.eq_ignore_ascii_case(&snapshot.server.database)) {
        return None;
    }
    Some(replica_status(r)).filter(|s| *s != ReplicaStatus::Fine)
}

/// An inactive replication slot with a current-to-restart WAL position distance of at least a gigabyte.
/// Inactivity does not establish abandonment, growth, disk occupancy or pressure. Only a native unreserved state
/// establishes that the slot has lost its reservation; a lost slot is handled separately above.
fn inactive_slot<'s>(snapshot: &Snapshot, r: &'s Replica) -> Option<InactiveSlot<'s>> {
    let kept = r.lag_bytes.filter(|b| *b >= GB)?;
    if r.condition != ReplicaCondition::SlotInactive || r.wal_status.as_deref() == Some("lost") {
        return None;
    }
    // safe_wal_size exists from 13 and is empty when nothing caps what slots keep.
    let capped = r.safe_wal_bytes.is_some();
    let (severity, actionability) = match r.wal_status.as_deref() {
        Some("unreserved") => (Severity::Medium, culprit_core::Actionability::NeedsAttention),
        _ => (Severity::Low, culprit_core::Actionability::Information),
    };
    Some(InactiveSlot {
        slot: r,
        kept,
        logical: r.is_logical(),
        capped,
        cap_known: r.wal_status.is_some(),
        // Before 13 nothing can cap a slot; from 13 the scan reads whether something does, unless the read failed.
        can_cap: culprit_core::version_at_least(&snapshot.server.version, [13, 0, 0]),
        severity,
        actionability,
    })
}

fn replica_lag_finding(snapshot: &Snapshot, lagging: Lagging<'_>) -> Finding {
    match lagging {
        Lagging::Replica { replica, status } => replica_finding(snapshot, replica, status),
        Lagging::Slot(slot) => slot_finding(slot),
        Lagging::LostSlot(slot) => lost_slot_finding(slot),
    }
}

/// A replica, a logical consumer or this server, stopped or behind.
fn replica_finding(snapshot: &Snapshot, r: &Replica, status: ReplicaStatus) -> Finding {
    let pg = snapshot.server.engine == Engine::Postgres;
    let mysql = snapshot.server.engine == Engine::Mysql;
    let consumer = r.is_logical();
    let seconds = r.lag_seconds;
    let mut f = finding("replica-lag", &r.name, Category::Configuration, subject(SubjectKind::Database, &r.name));
    let state = r.state_words().unwrap_or_default();
    match status {
        ReplicaStatus::ThreadsStopped => {
            f.title = t!("check.replica_lag.title.threads_stopped");
            f.summary = t!("check.replica_lag.summary.threads_stopped");
            f.severity = Severity::Low;
            f.actionability = culprit_core::Actionability::Information;
            f.fix = Some(Fix { summary: t!("check.replica_lag.fix.threads_stopped"), sql: None, cost: None });
        }
        ReplicaStatus::Stopped => {
            f.title = match r.role {
                ReplicaRole::ThisServer => t!("check.replica_lag.title.stopped_this_server", state = &state),
                _ if consumer => t!("check.replica_lag.title.stopped_consumer", name = &r.name, state = &state),
                _ => t!("check.replica_lag.title.stopped_replica", name = &r.name, state = &state),
            };
            let mut summary = Vec::new();
            if let Some(e) = &r.error {
                summary.push(t!("check.replica_lag.summary.last_error", error = e));
            }
            summary.push(if r.condition == ReplicaCondition::NotReceiving && r.role == ReplicaRole::ThisServer {
                t!("check.replica_lag.summary.no_receiver")
            } else {
                t!("check.replica_lag.summary.stopped")
            });
            f.summary = summary.join(" ");
            f.severity = Severity::High;
            f.fix = Some(Fix {
                summary: if mysql {
                    t!("check.replica_lag.fix.stopped_mysql")
                } else {
                    t!("check.replica_lag.fix.stopped")
                },
                sql: None,
                cost: None,
            });
        }
        ReplicaStatus::Fine => {}
        ReplicaStatus::Behind(lag) => {
            let lag = lag_words(lag);
            if consumer {
                f.title = t!("check.replica_lag.title.consumer_behind", name = &r.name, lag = &lag);
                f.summary = t!("check.replica_lag.summary.consumer_behind", name = &r.name, lag = &lag);
                f.fix = Some(Fix { summary: t!("check.replica_lag.fix.consumer_behind"), sql: None, cost: None });
            } else {
                f.title = match r.role {
                    ReplicaRole::ThisServer => t!("check.replica_lag.title.this_server_behind", lag = &lag),
                    _ => t!("check.replica_lag.title.replica_behind", name = &r.name, lag = &lag),
                };
                let mut summary = vec![if pg {
                    t!("check.replica_lag.summary.behind_replay", lag = &lag)
                } else {
                    t!("check.replica_lag.summary.behind", lag = &lag)
                }];
                if let Some(d) = r.delay_seconds {
                    summary.push(t!(
                        "check.replica_lag.summary.delayed",
                        delay = format::duration(d * 1000.0),
                        setting = if pg { "recovery_min_apply_delay" } else { "SOURCE_DELAY" }
                    ));
                }
                f.summary = summary.join(" ");
                f.fix = Some(Fix { summary: t!("check.replica_lag.fix.behind"), sql: None, cost: None });
            }
            f.severity = if seconds.is_some_and(|s| s >= 600.0) { Severity::High } else { Severity::Medium };
            if let Some(s) = seconds {
                f.metrics.push(Metric::said(
                    culprit_core::key!("check.replica_lag.metric.behind_by"),
                    s * 1000.0,
                    Unit::Ms,
                ));
            }
            if let Some(b) = r.lag_bytes {
                f.metrics.push(Metric::said(
                    culprit_core::key!("check.replica_lag.metric.not_applied"),
                    b,
                    Unit::Bytes,
                ));
            }
            if let Some(d) = r.delay_seconds {
                f.metrics.push(Metric::said(
                    culprit_core::key!("check.replica_lag.metric.delayed_by"),
                    d * 1000.0,
                    Unit::Ms,
                ));
            }
        }
    }
    severity_score(&mut f);
    f.impact.set_label(match status {
        ReplicaStatus::Behind(lag) => t!("check.replica_lag.impact.behind", lag = lag_words(lag)),
        _ => r.state_words().unwrap_or_else(|| t!("check.replica_lag.impact.stopped")),
    });
    f.confidence = Confidence::High;
    f.verify = vec![match status {
        ReplicaStatus::ThreadsStopped => t!("check.replica_lag.verify.threads_stopped"),
        _ => t!("check.replica_lag.verify.lag"),
    }];
    f
}

/// An inactive replication slot that keeps WAL.
fn slot_finding(slot: InactiveSlot<'_>) -> Finding {
    let InactiveSlot { slot: r, kept, logical, capped, cap_known, can_cap, .. } = slot;
    let mut f = finding("replica-lag", &r.name, Category::Configuration, subject(SubjectKind::Database, &r.name));
    f.title = if logical {
        t!("check.replica_lag.title.logical_slot", name = &r.name, kept = format::bytes(kept))
    } else {
        t!("check.replica_lag.title.physical_slot", name = &r.name, kept = format::bytes(kept))
    };
    let mut summary = vec![match r.inactive_seconds {
        Some(s) => t!("check.replica_lag.summary.slot_idle_for", name = &r.name, idle = format::duration(s * 1000.0)),
        None => t!("check.replica_lag.summary.slot_no_consumer", name = &r.name),
    }];
    summary.push(if logical {
        t!("check.replica_lag.summary.slot_logical")
    } else {
        t!("check.replica_lag.summary.slot_physical")
    });
    summary.push(match (capped, cap_known, can_cap) {
        (true, _, _) => {
            t!("check.replica_lag.summary.slot_capped", left = format::bytes(r.safe_wal_bytes.unwrap_or(0.0)))
        }
        (false, true, _) => t!("check.replica_lag.summary.slot_uncapped"),
        (false, false, true) => t!("check.replica_lag.summary.slot_cap_unknown"),
        (false, false, false) => t!("check.replica_lag.summary.slot_cannot_cap"),
    });
    f.summary = summary.join(" ");
    f.severity = slot.severity;
    f.actionability = slot.actionability;
    let quoted = r.name.replace('\'', "''");
    let sql = slot_inspection_sql(&quoted);
    let fix =
        if logical { t!("check.replica_lag.fix.logical_slot") } else { t!("check.replica_lag.fix.physical_slot") };
    f.fix = Some(Fix { summary: fix, sql: Some(sql), cost: None });
    f.metrics = vec![Metric::said(culprit_core::key!("check.replica_lag.metric.wal_kept"), kept, Unit::Bytes)];
    if let Some(s) = r.inactive_seconds {
        f.metrics.push(Metric::said(
            culprit_core::key!("check.replica_lag.metric.without_consumer"),
            s * 1000.0,
            Unit::Ms,
        ));
    }
    if let Some(b) = r.safe_wal_bytes {
        f.metrics.push(Metric::said(culprit_core::key!("check.replica_lag.metric.wal_left"), b, Unit::Bytes));
    }
    severity_score(&mut f);
    f.impact.set_label(t!("check.replica_lag.impact.wal_kept", kept = format::bytes(kept)));
    f.confidence = Confidence::High;
    f.verify = vec![if logical {
        t!("check.replica_lag.verify.logical_slot")
    } else {
        t!("check.replica_lag.verify.physical_slot")
    }];
    f
}

fn lost_slot_finding(slot: &Replica) -> Finding {
    let mut f = finding("replica-lag", &slot.name, Category::Configuration, subject(SubjectKind::Database, &slot.name));
    f.title = t!("check.replica_lag.title.lost_slot", name = &slot.name);
    f.summary = t!("check.replica_lag.summary.lost_slot");
    f.impact.set_label(t!("check.replica_lag.impact.lost_slot"));
    f.severity = Severity::Medium;
    f.actionability = culprit_core::Actionability::NeedsAttention;
    f.confidence = Confidence::High;
    f.fix = Some(Fix {
        summary: t!("check.replica_lag.fix.lost_slot"),
        sql: Some(slot_inspection_sql(&slot.name.replace('\'', "''"))),
        cost: None,
    });
    f.verify = vec![t!("check.replica_lag.verify.lost_slot")];
    severity_score(&mut f);
    f
}

fn slot_inspection_sql(quoted_name: &str) -> String {
    format!(
        "SELECT slot_name,slot_type,active,active_pid,restart_lsn,confirmed_flush_lsn \
        ,pg_catalog.to_jsonb(s)->>'wal_status' AS wal_status \
        ,pg_catalog.to_jsonb(s)->>'invalidation_reason' AS invalidation_reason \
        FROM pg_catalog.pg_replication_slots s WHERE slot_name='{quoted_name}';"
    )
}
