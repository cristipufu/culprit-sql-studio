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
        t!("view.maintenance_progress.limit.background"),
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
    if snapshot.observations.iter().any(|row| matches!(row.work, MaintenanceProgressWork::YsqlIndexBuild { .. })) {
        limitations.push(t!("view.maintenance_progress.limit.ysql"));
    }
    if snapshot.observations.iter().any(|row| {
        matches!(&row.work, MaintenanceProgressWork::PostgreSqlIndexBuild { progress } if progress.current_locker_pid.as_deref().is_some_and(|pid| pid != "0"))
    }) {
        limitations.push(t!("view.maintenance_progress.limit.locker"));
    }
    if snapshot.observations.iter().any(|row| match &row.work {
        MaintenanceProgressWork::PostgreSqlVacuum { progress } => progress.cost_delay_ms.is_some(),
        MaintenanceProgressWork::PostgreSqlAnalyze { progress } => progress.cost_delay_ms.is_some(),
        _ => false,
    }) {
        limitations.push(t!("view.maintenance_progress.limit.cost_delay"));
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
        FocusedRequestBinding::ParticipantMissing => t!("view.maintenance_progress.summary.missing"),
        FocusedRequestBinding::ParticipantChanged => t!("view.maintenance_progress.summary.changed"),
        _ => match snapshot.result.membership {
            FocusedRequestMembership::Unavailable => t!("view.maintenance_progress.summary.unavailable"),
            FocusedRequestMembership::NotObserved => t!("view.maintenance_progress.summary.not_observed"),
            FocusedRequestMembership::Observed => match snapshot.result.binding {
                FocusedRequestBinding::Matched => t!("view.maintenance_progress.summary.matched"),
                FocusedRequestBinding::UnverifiedOriginalScope => {
                    t!("view.maintenance_progress.summary.unverified")
                }
                _ => t!("view.maintenance_progress.summary.independent"),
            },
        },
    }
}

fn entry(row: &MaintenanceProgressObservation) -> Result<MaintenanceProgressEntry> {
    let (native_explanation, metrics) = explain_work(&row.work);
    let wait = row.postgres_index_wait();
    let (explanation, investigation, verify) = match &wait {
        Some(wait) => explain_wait(wait.phase),
        None => {
            (native_explanation, t!("view.maintenance_progress.investigate"), t!("view.maintenance_progress.verify"))
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
        wait,
        title: t!("view.maintenance_progress.entry.title", session = &row.actor.session.id),
        explanation,
        metrics,
        investigation,
        verify,
    })
}

fn explain_wait(phase: PostgresIndexWaitPhase) -> (String, String, String) {
    let (explanation, investigation) = match phase {
        PostgresIndexWaitPhase::WritersBeforeBuild => (
            t!("view.maintenance_progress.wait.writers_before_build"),
            t!("view.maintenance_progress.wait.investigate_writers"),
        ),
        PostgresIndexWaitPhase::WritersBeforeValidation => (
            t!("view.maintenance_progress.wait.writers_before_validation"),
            t!("view.maintenance_progress.wait.investigate_writers"),
        ),
        PostgresIndexWaitPhase::OldSnapshots => (
            t!("view.maintenance_progress.wait.old_snapshots"),
            t!("view.maintenance_progress.wait.investigate_snapshots"),
        ),
        PostgresIndexWaitPhase::ReadersBeforeMarkingDead => (
            t!("view.maintenance_progress.wait.readers_before_marking_dead"),
            t!("view.maintenance_progress.wait.investigate_readers"),
        ),
        PostgresIndexWaitPhase::ReadersBeforeDropping => (
            t!("view.maintenance_progress.wait.readers_before_dropping"),
            t!("view.maintenance_progress.wait.investigate_readers"),
        ),
    };
    (explanation, investigation, t!("view.maintenance_progress.wait.verify"))
}

fn phase_metrics<'a>(fields: impl IntoIterator<Item = (&'a str, &'a Option<String>)>) -> Vec<Metric> {
    fields
        .into_iter()
        .filter_map(|(label, value)| {
            value.as_ref().map(|value| Metric::said_text(label, format::digits(value.clone())))
        })
        .collect()
}

fn explain_work(work: &MaintenanceProgressWork) -> (String, Vec<Metric>) {
    match work {
        MaintenanceProgressWork::PostgreSqlIndexBuild { progress: p } => (
            t!("view.maintenance_progress.entry.postgres"),
            phase_metrics([
                (key!("view.maintenance_progress.metric.blocks_done"), &p.blocks_done),
                (key!("view.maintenance_progress.metric.blocks_total"), &p.blocks_total),
                (key!("view.maintenance_progress.metric.tuples_done"), &p.tuples_done),
                (key!("view.maintenance_progress.metric.tuples_total"), &p.tuples_total),
                (key!("view.maintenance_progress.metric.lockers_done"), &p.lockers_done),
                (key!("view.maintenance_progress.metric.lockers_total"), &p.lockers_total),
                (key!("view.maintenance_progress.metric.locker_pid"), &p.current_locker_pid),
            ]),
        ),
        MaintenanceProgressWork::YsqlIndexBuild { progress: p } => (
            t!("view.maintenance_progress.entry.ysql"),
            phase_metrics([
                (key!("view.maintenance_progress.metric.ysql_done"), &p.tuples_done),
                (key!("view.maintenance_progress.metric.ysql_estimate"), &p.tuples_total_estimate),
            ]),
        ),
        MaintenanceProgressWork::PostgreSqlVacuum { progress: p } => (
            t!("view.maintenance_progress.entry.vacuum"),
            phase_metrics([
                (key!("view.maintenance_progress.metric.heap_blocks_total"), &p.heap_blks_total),
                (key!("view.maintenance_progress.metric.heap_blocks_scanned"), &p.heap_blks_scanned),
                (key!("view.maintenance_progress.metric.heap_blocks_vacuumed"), &p.heap_blks_vacuumed),
                (key!("view.maintenance_progress.metric.index_vacuum_cycles"), &p.index_vacuum_count),
                (key!("view.maintenance_progress.metric.legacy_dead_tuple_capacity"), &p.max_dead_tuples),
                (key!("view.maintenance_progress.metric.legacy_dead_tuples"), &p.num_dead_tuples),
                (key!("view.maintenance_progress.metric.vacuum_indexes_total"), &p.indexes_total),
                (key!("view.maintenance_progress.metric.vacuum_indexes_processed"), &p.indexes_processed),
                (key!("view.maintenance_progress.metric.dead_item_capacity_bytes"), &p.max_dead_tuple_bytes),
                (key!("view.maintenance_progress.metric.dead_item_bytes"), &p.dead_tuple_bytes),
                (key!("view.maintenance_progress.metric.dead_item_ids"), &p.num_dead_item_ids),
                (key!("view.maintenance_progress.metric.cost_delay_ms"), &p.cost_delay_ms),
            ]),
        ),
        MaintenanceProgressWork::PostgreSqlAnalyze { progress: p } => (
            t!("view.maintenance_progress.entry.analyze"),
            phase_metrics([
                (key!("view.maintenance_progress.metric.sample_blocks_total"), &p.sample_blks_total),
                (key!("view.maintenance_progress.metric.sample_blocks_scanned"), &p.sample_blks_scanned),
                (key!("view.maintenance_progress.metric.extended_statistics_total"), &p.ext_stats_total),
                (key!("view.maintenance_progress.metric.extended_statistics_computed"), &p.ext_stats_computed),
                (key!("view.maintenance_progress.metric.child_tables_total"), &p.child_tables_total),
                (key!("view.maintenance_progress.metric.child_tables_done"), &p.child_tables_done),
                (key!("view.maintenance_progress.metric.child_table_oid"), &p.current_child_table_relid),
                (key!("view.maintenance_progress.metric.cost_delay_ms"), &p.cost_delay_ms),
            ]),
        ),
        MaintenanceProgressWork::PostgreSqlCluster { progress: p } => (
            t!("view.maintenance_progress.entry.cluster"),
            phase_metrics([
                (key!("view.maintenance_progress.metric.heap_tuples_scanned"), &p.heap_tuples_scanned),
                (key!("view.maintenance_progress.metric.heap_tuples_written"), &p.heap_tuples_written),
                (key!("view.maintenance_progress.metric.heap_blocks_total"), &p.heap_blks_total),
                (key!("view.maintenance_progress.metric.heap_blocks_scanned"), &p.heap_blks_scanned),
                (key!("view.maintenance_progress.metric.index_rebuilds"), &p.index_rebuild_count),
            ]),
        ),
    }
}

#[cfg(test)]
mod tests;
