//! A native maintenance point retains its actor and phase, without becoming completion or lock-owner history.
use culprit_core::{
    Error, MaintenanceProgressEntry, MaintenanceProgressReport, Metric, Result, format, key, t, telemetry::*,
};

pub fn explain(snapshot: &MaintenanceProgressSnapshot, read_complete: bool) -> Result<MaintenanceProgressReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(Error::invalid(t!("view.maintenance_progress.invalid")));
    }
    let mut limitations = vec![
        t!("view.maintenance_progress.limit.point"),
        t!("view.maintenance_progress.limit.cause"),
        t!("view.maintenance_progress.limit.objects"),
        t!("view.maintenance_progress.limit.disappearance"),
    ];
    if !snapshot.context.has_qualified_local_owner() {
        limitations.push(t!("view.maintenance_progress.limit.current_scope"));
    }
    if !read_complete || snapshot.observations.len() != snapshot.coverage.records as usize {
        limitations.push(t!("view.maintenance_progress.limit.read"));
    }
    if !snapshot.coverage.complete_scope {
        limitations.push(t!("view.maintenance_progress.limit.sampled"));
    }
    if snapshot.observations.iter().any(|row| matches!(row.work, IndexBuildProgressWork::Ysql { .. })) {
        limitations.push(t!("view.maintenance_progress.limit.ysql"));
    }
    if snapshot.observations.iter().any(|row| {
        matches!(&row.work, IndexBuildProgressWork::PostgreSql { progress } if progress.current_locker_pid.as_deref().is_some_and(|pid| pid != "0"))
    }) {
        limitations.push(t!("view.maintenance_progress.limit.locker"));
    }
    Ok(MaintenanceProgressReport {
        source: snapshot.source.clone(),
        context: snapshot.context.clone(),
        source_state: snapshot.coverage.state,
        source_reason: None,
        read_complete,
        native_rows: snapshot.coverage.records,
        observed_rows: snapshot.observations.len() as u32,
        binding: snapshot.result.binding,
        membership: snapshot.result.membership,
        title: t!("view.maintenance_progress.title"),
        summary: summary(snapshot),
        limitations,
        entries: snapshot.observations.iter().map(entry).collect::<Result<_>>()?,
    })
}

fn summary(snapshot: &MaintenanceProgressSnapshot) -> String {
    match snapshot.result.binding {
        MaintenanceProgressBinding::ParticipantMissing => t!("view.maintenance_progress.summary.missing"),
        MaintenanceProgressBinding::ParticipantChanged => t!("view.maintenance_progress.summary.changed"),
        _ => match snapshot.result.membership {
            MaintenanceProgressMembership::Unavailable => t!("view.maintenance_progress.summary.unavailable"),
            MaintenanceProgressMembership::NotObserved => t!("view.maintenance_progress.summary.not_observed"),
            MaintenanceProgressMembership::Observed => match snapshot.result.binding {
                MaintenanceProgressBinding::Matched => t!("view.maintenance_progress.summary.matched"),
                MaintenanceProgressBinding::UnverifiedOriginalScope => {
                    t!("view.maintenance_progress.summary.unverified")
                }
                _ => t!("view.maintenance_progress.summary.independent"),
            },
        },
    }
}

fn entry(row: &MaintenanceProgressObservation) -> Result<MaintenanceProgressEntry> {
    let (explanation, metrics) = match &row.work {
        IndexBuildProgressWork::PostgreSql { progress } => {
            let mut metrics = Vec::new();
            for (label, value) in [
                (key!("view.maintenance_progress.metric.blocks_done"), &progress.blocks_done),
                (key!("view.maintenance_progress.metric.blocks_total"), &progress.blocks_total),
                (key!("view.maintenance_progress.metric.tuples_done"), &progress.tuples_done),
                (key!("view.maintenance_progress.metric.tuples_total"), &progress.tuples_total),
                (key!("view.maintenance_progress.metric.lockers_done"), &progress.lockers_done),
                (key!("view.maintenance_progress.metric.lockers_total"), &progress.lockers_total),
                (key!("view.maintenance_progress.metric.locker_pid"), &progress.current_locker_pid),
            ] {
                if let Some(value) = value {
                    metrics.push(Metric::said_text(label, format::digits(value.clone())));
                }
            }
            (t!("view.maintenance_progress.entry.postgres"), metrics)
        }
        IndexBuildProgressWork::Ysql { progress } => {
            let mut metrics = Vec::new();
            for (label, value) in [
                (key!("view.maintenance_progress.metric.ysql_done"), &progress.tuples_done),
                (key!("view.maintenance_progress.metric.ysql_estimate"), &progress.tuples_total_estimate),
            ] {
                if let Some(value) = value {
                    metrics.push(Metric::said_text(label, format::digits(value.clone())));
                }
            }
            (t!("view.maintenance_progress.entry.ysql"), metrics)
        }
    };
    Ok(MaintenanceProgressEntry {
        observation_key: row.key()?,
        actor: row.actor.clone(),
        database_oid: row.database_oid.clone(),
        relation: row.relation_reference(),
        index: row.index_reference(),
        command: row.command,
        phase: row.phase.clone(),
        title: t!("view.maintenance_progress.entry.title", session = &row.actor.session.id),
        explanation,
        metrics,
        investigation: t!("view.maintenance_progress.investigate"),
        verify: t!("view.maintenance_progress.verify"),
    })
}

#[cfg(test)]
mod tests;
