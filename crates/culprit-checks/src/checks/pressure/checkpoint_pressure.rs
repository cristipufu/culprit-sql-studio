//! Requested checkpoints, with no inference of WAL pressure from the shared counter alone.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn checkpoint_pressure_info() -> CheckInfo {
    CheckInfo {
        id: "checkpoint-pressure".into(),
        title: t!("check.checkpoint_pressure.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Postgres],
        requires: vec![],
        description: t!("check.checkpoint_pressure.info.description"),
    }
}

check!(
    CheckpointPressureCheck: checkpoint_pressure_info,
    decide = requested_checkpoints,
    render = checkpoint_pressure_finding,
    case<'s> = RequestedCheckpoints<'s>,
    evidence = &[Evidence::Pressure {
        signal: Some(culprit_core::signal_id::CHECKPOINTS_REQUESTED),
        timed_files: false,
        complete: true
    }],
);

/// The checkpoint-pressure findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn checkpoint_pressure(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&CheckpointPressureCheck, snapshot)
}

/// The server's checkpoints, many of them requested rather than timed, and the WAL room a confirmed cause would need.
pub struct RequestedCheckpoints<'s> {
    /// Requested checkpoints, as a percentage of all of them.
    requested: f64,
    /// Checkpoints since the counters were reset.
    total: f64,
    /// max_wal_size as the server reports it.
    current: Option<&'s str>,
    /// More room than it has now: 8GB, or twice the current size when that's already at least 8GB.
    proposed: String,
}

fn requested_checkpoints(snapshot: &Snapshot) -> Vec<RequestedCheckpoints<'_>> {
    let (Some(requested), Some(total)) = (
        signal(snapshot, culprit_core::signal_id::CHECKPOINTS_REQUESTED),
        signal(snapshot, culprit_core::signal_id::CHECKPOINTS),
    ) else {
        return Vec::new();
    };
    if requested < 30.0 || total < 10.0 || snapshot.server.platform().is_some_and(|p| p.applies_wal_in_storage()) {
        return Vec::new();
    }
    let current = snapshot.settings.iter().find(|s| s.name == "max_wal_size").map(|s| s.value.as_str());
    let proposed_mb = current.and_then(size_in_mb).map_or(8192.0, |mb| (mb * 2.0).max(8192.0));
    let proposed = if proposed_mb >= 1024.0 && proposed_mb % 1024.0 == 0.0 {
        format!("{:.0}GB", proposed_mb / 1024.0)
    } else {
        format!("{proposed_mb:.0}MB")
    };
    vec![RequestedCheckpoints { requested, total, current, proposed }]
}

fn checkpoint_pressure_finding(snapshot: &Snapshot, c: RequestedCheckpoints<'_>) -> Finding {
    let mut f = finding(
        "checkpoint-pressure",
        "max_wal_size",
        Category::Configuration,
        subject(SubjectKind::Setting, "max_wal_size"),
    );
    // PostgreSQL counts every checkpoint something asked for as requested: WAL reaching max_wal_size, but also a
    // manual CHECKPOINT, a base backup or CREATE DATABASE. The counter doesn't say which.
    let percent = format!("{:.0}", c.requested);
    f.title = t!("check.checkpoint_pressure.title", percent = &percent);
    f.summary = [
        t!(
            "check.checkpoint_pressure.summary.requested",
            requested = format::count(c.total * c.requested / 100.0),
            total = format::count(c.total)
        ),
        match c.current {
            Some(size) => t!("check.checkpoint_pressure.summary.causes_at", size = size),
            None => t!("check.checkpoint_pressure.summary.causes"),
        },
        t!("check.checkpoint_pressure.summary.not_proof"),
    ]
    .join(" ");
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + c.requested / 10.0;
    f.impact.label = t!("check.checkpoint_pressure.impact", percent = &percent);
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::new(t!("check.checkpoint_pressure.metric.requested"), c.requested / 100.0, Unit::Ratio),
        Metric::new(t!("check.checkpoint_pressure.metric.checkpoints"), c.total, Unit::Count),
    ];
    let change = setting_change(&snapshot.server, "max_wal_size", &c.proposed);
    f.fix = Some(Fix {
        summary: t!("check.checkpoint_pressure.fix", how = change.how),
        sql: None,
        cost: Some(t!("check.checkpoint_pressure.fix_cost")),
    });
    f.verify = vec![t!("check.checkpoint_pressure.verify")];
    f
}
