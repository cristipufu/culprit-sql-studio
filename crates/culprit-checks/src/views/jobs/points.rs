use super::*;
pub(super) fn priority(fact: &OperationalFact) -> u8 {
    match fact {
        OperationalFact::TimescaleJobRun(point) if point.succeeded == Some(false) && point.finished_at.is_some() => 0,
        OperationalFact::TimescaleJobState(point)
            if point
                .statistics
                .as_ref()
                .is_some_and(|stats| stats.last_run_status == Some(TimescaleJobLastRun::Failed)) =>
        {
            1
        }
        OperationalFact::TimescaleJobRun(_) => 2,
        _ => 3,
    }
}
pub(super) fn state(row: &OperationalObservation, point: &TimescaleJobState, duplicate: bool) -> JobEntry {
    let mut explanation = match point.statistics.as_ref().and_then(|stats| stats.reported_job_state.as_ref()) {
        Some(TimescaleReportedJobState::Running) => t!("view.jobs.state.running"),
        Some(TimescaleReportedJobState::Scheduled) => t!("view.jobs.state.scheduled"),
        Some(TimescaleReportedJobState::Paused) => t!("view.jobs.state.paused"),
        _ => t!("view.jobs.state.unknown"),
    };
    if duplicate {
        explanation.push(' ');
        explanation.push_str(&t!("view.jobs.limit.duplicates"));
    }
    let mut metrics = vec![
        Metric::said_text(culprit_core::key!("view.jobs.metric.observed_at"), &point.observed_at),
        Metric::said_text(culprit_core::key!("view.jobs.metric.scheduled"), point.scheduled.to_string()),
        Metric::said_text(
            culprit_core::key!("view.jobs.metric.procedure"),
            format!("{}.{}", point.procedure.schema, point.procedure.name),
        ),
        Metric::said_text(culprit_core::key!("view.jobs.metric.owner"), &point.owner),
        Metric::said_text(culprit_core::key!("view.jobs.metric.schedule_interval"), &point.schedule_interval),
        Metric::said_text(culprit_core::key!("view.jobs.metric.max_runtime"), &point.max_runtime),
        Metric::said_text(culprit_core::key!("view.jobs.metric.retry_period"), &point.retry_period),
        Metric::said_text(culprit_core::key!("view.jobs.metric.max_retries"), &point.max_retries),
    ];
    if let Some(fixed) = point.fixed_schedule {
        metrics.push(Metric::said_text(culprit_core::key!("view.jobs.metric.fixed_schedule"), fixed.to_string()));
    }
    for (key, value) in [
        (culprit_core::key!("view.jobs.metric.initial_start"), point.initial_start.as_ref()),
        (culprit_core::key!("view.jobs.metric.next_start"), point.next_start.as_ref()),
    ] {
        if let Some(value) = value {
            metrics.push(Metric::said_text(key, timestamp(value)));
        }
    }
    if let Some(object) = &point.hypertable {
        metrics.push(Metric::said_text(
            culprit_core::key!("view.jobs.metric.associated_object"),
            format!("{}.{}", object.schema, object.name),
        ));
    }
    if let Some(stats) = &point.statistics {
        if let Some(status) = &stats.last_run_status {
            let text = match status {
                TimescaleJobLastRun::Success => t!("view.jobs.status.success"),
                TimescaleJobLastRun::Failed => t!("view.jobs.status.failed"),
                TimescaleJobLastRun::Unknown { native } => native.clone(),
            };
            metrics.push(Metric::said_text(culprit_core::key!("view.jobs.metric.last_status"), text));
        }
        if let Some(TimescaleReportedJobState::Unknown { native }) = &stats.reported_job_state {
            metrics.push(Metric::said_text(culprit_core::key!("view.jobs.metric.native_status"), native));
        }
        for (key, value) in [
            (culprit_core::key!("view.jobs.metric.total_runs"), &stats.total_runs),
            (culprit_core::key!("view.jobs.metric.total_successes"), &stats.total_successes),
            (culprit_core::key!("view.jobs.metric.total_failures"), &stats.total_failures),
            (culprit_core::key!("view.jobs.metric.last_duration"), &stats.last_run_duration),
        ] {
            if let Some(value) = value {
                metrics.push(Metric::said_text(key, value));
            }
        }
        for (key, value) in [
            (culprit_core::key!("view.jobs.metric.last_start"), stats.last_run_started_at.as_ref()),
            (culprit_core::key!("view.jobs.metric.last_success"), stats.last_successful_finish.as_ref()),
            (culprit_core::key!("view.jobs.metric.stats_next_start"), stats.next_start.as_ref()),
        ] {
            if let Some(value) = value {
                metrics.push(Metric::said_text(key, timestamp(value)));
            }
        }
    } else {
        explanation.push(' ');
        explanation.push_str(&t!("view.jobs.state.no_statistics"));
    }
    entry(
        row,
        &point.job_id,
        JobEntryKind::State,
        t!("view.jobs.state.title", job = &point.job_id),
        explanation,
        metrics,
    )
}
pub(super) fn run(row: &OperationalObservation, point: &TimescaleJobRun) -> JobEntry {
    let (title, mut explanation) = match (point.succeeded, point.finished_at.as_ref()) {
        (Some(false), Some(_)) => {
            let mut explanation = t!("view.jobs.run.failed");
            if let Some(kind) = point.failure_kind() {
                let (_, native, _) = super::super::native_failures::postgres_explanation(kind, None);
                explanation.push(' ');
                explanation.push_str(&native);
            }
            (t!("view.jobs.run.failed_title", job = &point.job_id), explanation)
        }
        (Some(true), Some(_)) => (t!("view.jobs.run.success_title", job = &point.job_id), t!("view.jobs.run.success")),
        _ => (t!("view.jobs.run.unverified_title", job = &point.job_id), t!("view.jobs.run.unverified")),
    };
    let mut metrics = vec![
        Metric::said_text(culprit_core::key!("view.jobs.metric.observed_at"), &point.observed_at),
        Metric::said_text(culprit_core::key!("view.jobs.metric.started_at"), &point.started_at),
    ];
    if let Some(succeeded) = point.succeeded {
        metrics.push(Metric::said_text(culprit_core::key!("view.jobs.metric.succeeded"), succeeded.to_string()));
    }
    for (key, value) in [
        (culprit_core::key!("view.jobs.metric.run_id"), &point.native_run_id),
        (culprit_core::key!("view.jobs.metric.finished_at"), &point.finished_at),
        (culprit_core::key!("view.jobs.metric.process_id"), &point.process_id),
        (culprit_core::key!("view.jobs.metric.sql_state"), &point.sql_state),
    ] {
        if let Some(value) = value {
            metrics.push(Metric::said_text(key, value));
        }
    }
    if let Some(procedure) = &point.procedure {
        metrics.push(Metric::said_text(
            culprit_core::key!("view.jobs.metric.reported_procedure"),
            format!("{}.{}", procedure.label.schema, procedure.label.name),
        ));
        explanation.push(' ');
        explanation.push_str(&if procedure.origin == TimescaleJobProcedureOrigin::Stored {
            t!("view.jobs.limit.stored_label")
        } else {
            t!("view.jobs.limit.labels")
        });
    }
    entry(row, &point.job_id, JobEntryKind::RetainedRun, title, explanation, metrics)
}
