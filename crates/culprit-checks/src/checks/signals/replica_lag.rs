//! The `replica-lag` check: Replicas far behind the primary or stopped, and PostgreSQL replication slots that keep WAL for a consumer that's gone: pg_stat_replication, availability group replica states, SHOW REPLICA STATUS.

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
pub(super) enum ReplicaStatus {
    /// Replication failed or can't connect: the copy stopped with an error, or a standby meant to stream has no
    /// receiver.
    Stopped,
    /// Both MySQL replication threads are off and nothing failed: someone ran STOP REPLICA.
    StoppedByHand,
    /// The same on a server that takes writes: a former replica promoted to primary, its old configuration left behind.
    LeftConfigured,
    /// Behind by this many seconds beyond any configured delay (or by bytes, when that is all the engine says).
    Behind,
    Fine,
}

pub(super) fn replica_status(r: &Replica) -> ReplicaStatus {
    // Group Replication's own channels idle by design; the group reports its members elsewhere.
    if r.name.to_ascii_lowercase().starts_with("group_replication_") {
        return ReplicaStatus::Fine;
    }
    let state = r.state.as_deref().unwrap_or("");
    if state.starts_with("configured, not running") {
        return ReplicaStatus::LeftConfigured;
    }
    if state.starts_with("stopped by hand") {
        return ReplicaStatus::StoppedByHand;
    }
    if state.contains("stopped") || state == "not receiving" || state.eq_ignore_ascii_case("NOT SYNCHRONIZING") {
        return ReplicaStatus::Stopped;
    }
    let seconds = r.lag_seconds;
    // A configured delay keeps WAL waiting on purpose: only the seconds beyond it say whether it is behind.
    let bytes = r.lag_bytes.filter(|_| r.delay_seconds.is_none() && seconds.is_none_or(|s| s > 0.0));
    if seconds.is_some_and(|s| s >= 30.0) || bytes.is_some_and(|b| b >= GB) {
        ReplicaStatus::Behind
    } else {
        ReplicaStatus::Fine
    }
}

/// A replica, a logical consumer or this server that is stopped or behind, or a slot that keeps WAL for nothing.
pub enum Lagging<'s> {
    Replica { replica: &'s Replica, status: ReplicaStatus },
    Slot(InactiveSlot<'s>),
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
}

fn lagging(snapshot: &Snapshot) -> Vec<Lagging<'_>> {
    let Some(p) = snapshot.pressure.as_ref() else { return Vec::new() };
    p.replicas
        .iter()
        .filter_map(|r| match r.role {
            ReplicaRole::Slot => inactive_slot(snapshot, r).map(Lagging::Slot),
            _ => stopped_or_behind(snapshot, r).map(|status| Lagging::Replica { replica: r, status }),
        })
        .collect()
}

/// How a replica, a logical consumer or this server stands, when it is stopped or behind.
pub(super) fn stopped_or_behind(snapshot: &Snapshot, r: &Replica) -> Option<ReplicaStatus> {
    // A logical consumer of another database on this server is that database's lag (its slot, kept server-wide, is
    // still reported as a slot).
    if r.is_logical() && r.database.as_deref().is_some_and(|d| !d.eq_ignore_ascii_case(&snapshot.server.database)) {
        return None;
    }
    Some(replica_status(r)).filter(|s| *s != ReplicaStatus::Fine)
}

/// An inactive replication slot that keeps a gigabyte or more of WAL. A physical slot without its replica keeps WAL for
/// nothing; a logical one belongs to a consumer (a subscription, a CDC tool) that may only be between syncs, and
/// dropping it forces that consumer to copy everything again. Whether it can fill the disk depends on
/// max_slot_wal_keep_size (13+): with a cap, PostgreSQL gives the slot up instead.
pub(super) fn inactive_slot<'s>(snapshot: &Snapshot, r: &'s Replica) -> Option<InactiveSlot<'s>> {
    let kept = r.lag_bytes.filter(|b| *b >= GB)?;
    if r.state.as_deref() != Some("inactive") || r.wal_status.as_deref() == Some("lost") {
        return None;
    }
    // safe_wal_size exists from 13 and is empty when nothing caps what slots keep.
    let capped = r.safe_wal_bytes.is_some();
    let severity = match (capped, r.wal_status.as_deref()) {
        (true, Some("unreserved")) => Severity::Medium,
        (true, _) => Severity::Low,
        (false, _) if kept >= 10.0 * GB => Severity::High,
        _ => Severity::Medium,
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
    })
}

fn replica_lag_finding(snapshot: &Snapshot, lagging: Lagging<'_>) -> Finding {
    match lagging {
        Lagging::Replica { replica, status } => replica_finding(snapshot, replica, status),
        Lagging::Slot(slot) => slot_finding(snapshot, slot),
    }
}

/// A replica, a logical consumer or this server, stopped or behind.
fn replica_finding(snapshot: &Snapshot, r: &Replica, status: ReplicaStatus) -> Finding {
    let pg = snapshot.server.engine == Engine::Postgres;
    let mysql = snapshot.server.engine == Engine::Mysql;
    let consumer = r.is_logical();
    let seconds = r.lag_seconds;
    let mut f = finding("replica-lag", &r.name, Category::Configuration, subject(SubjectKind::Database, &r.name));
    let lag = seconds
        .map(|s| format::duration(s * 1000.0))
        .or_else(|| r.lag_bytes.map(|b| t!("check.replica_lag.lag_bytes", bytes = format::bytes(b))))
        .unwrap_or_else(|| t!("check.replica_lag.lag_unknown"));
    let state = r.state.clone().unwrap_or_default();
    match status {
        ReplicaStatus::LeftConfigured => {
            f.title = t!("check.replica_lag.title.left_configured");
            f.summary = t!("check.replica_lag.summary.left_configured");
            f.severity = Severity::Low;
            f.fix = Some(Fix { summary: t!("check.replica_lag.fix.left_configured"), sql: None, cost: None });
        }
        ReplicaStatus::StoppedByHand => {
            f.title = t!("check.replica_lag.title.stopped_by_hand");
            f.summary = t!("check.replica_lag.summary.stopped_by_hand");
            f.severity = Severity::Medium;
            f.fix = Some(Fix { summary: t!("check.replica_lag.fix.stopped_by_hand"), sql: None, cost: None });
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
            summary.push(if pg && r.role == ReplicaRole::ThisServer && state == "not receiving" {
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
        _ => {
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
                f.metrics.push(Metric::new(t!("check.replica_lag.metric.behind_by"), s * 1000.0, Unit::Ms));
            }
            if let Some(b) = r.lag_bytes {
                f.metrics.push(Metric::new(t!("check.replica_lag.metric.not_applied"), b, Unit::Bytes));
            }
            if let Some(d) = r.delay_seconds {
                f.metrics.push(Metric::new(t!("check.replica_lag.metric.delayed_by"), d * 1000.0, Unit::Ms));
            }
        }
    }
    severity_score(&mut f);
    f.impact.label = match status {
        ReplicaStatus::LeftConfigured | ReplicaStatus::StoppedByHand | ReplicaStatus::Stopped => {
            r.state.clone().unwrap_or_else(|| t!("check.replica_lag.impact.stopped"))
        }
        _ => t!("check.replica_lag.impact.behind", lag = &lag),
    };
    f.confidence = Confidence::High;
    f.verify = vec![match status {
        ReplicaStatus::LeftConfigured => t!("check.replica_lag.verify.left_configured"),
        _ => t!("check.replica_lag.verify.lag"),
    }];
    f
}

/// An inactive replication slot that keeps WAL.
fn slot_finding(snapshot: &Snapshot, slot: InactiveSlot<'_>) -> Finding {
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
    let quoted = r.name.replace('\'', "''");
    // Only where nothing caps slots yet (or the scan couldn't tell): a cap already set is never lowered.
    let cap = (!capped && can_cap).then(|| setting_change(&snapshot.server, "max_slot_wal_keep_size", "50GB"));
    let mut sql = if logical {
        format!(
            "-- Only once its consumer is gone for good: dropping it makes the consumer copy everything again\n-- SELECT pg_drop_replication_slot('{}');",
            crate::support::comment_text(&quoted)
        )
    } else {
        format!("SELECT pg_drop_replication_slot('{quoted}');")
    };
    if let Some(statements) = cap.as_ref().and_then(|c| c.sql.as_deref()) {
        let lead = if cap_known {
            "Cap what any slot may keep"
        } else {
            "If max_slot_wal_keep_size isn't set, cap what any slot may keep"
        };
        sql.push_str(&format!("\n-- {lead}:\n{statements}"));
    }
    let mut fix = vec![if logical {
        t!("check.replica_lag.fix.logical_slot")
    } else {
        t!("check.replica_lag.fix.physical_slot")
    }];
    match &cap {
        Some(c) if cap_known => fix.push(t!("check.replica_lag.fix.cap", how = &c.how)),
        Some(c) => fix.push(t!("check.replica_lag.fix.cap_if_unset", how = &c.how)),
        None if !can_cap => fix.push(t!("check.replica_lag.fix.cap_from_13")),
        None => {}
    }
    f.fix = Some(Fix { summary: fix.join(" "), sql: Some(sql), cost: None });
    f.metrics = vec![Metric::new(t!("check.replica_lag.metric.wal_kept"), kept, Unit::Bytes)];
    if let Some(s) = r.inactive_seconds {
        f.metrics.push(Metric::new(t!("check.replica_lag.metric.without_consumer"), s * 1000.0, Unit::Ms));
    }
    if let Some(b) = r.safe_wal_bytes {
        f.metrics.push(Metric::new(t!("check.replica_lag.metric.wal_left"), b, Unit::Bytes));
    }
    severity_score(&mut f);
    f.impact.label = t!("check.replica_lag.impact.wal_kept", kept = format::bytes(kept));
    f.confidence = Confidence::High;
    f.verify = vec![if logical {
        t!("check.replica_lag.verify.logical_slot")
    } else {
        t!("check.replica_lag.verify.physical_slot")
    }];
    f
}
