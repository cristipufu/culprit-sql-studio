//! A retained failure is historical; Agent service/session and reported job activity never identify a SQL owner.
use super::*;

pub(super) fn explain(read: &MssqlAgentJobsSnapshot, read_complete: bool, max_entries: usize) -> Result<JobsReport> {
    if !read.is_valid_read(read_complete) {
        return Err(invalid());
    }
    let mut rows = read.observations.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (priority(&left.fact), &left.source, &left.key).cmp(&(priority(&right.fact), &right.source, &right.key))
    });
    let limit = max_entries.min(MAX_JOB_ENTRIES);
    let entries = rows.iter().take(limit).map(|row| entry(row)).collect::<Result<Vec<_>>>()?;
    let mut counts = BTreeMap::<&str, usize>::new();
    for row in &read.observations {
        *counts.entry(&row.source).or_default() += 1;
    }
    let sources = read
        .sources
        .iter()
        .map(|source| outcome(source, counts.get(source.source.as_str()).copied().unwrap_or(0), read_complete))
        .collect();
    let mut limitations = if read.metadata.availability == MssqlAgentAvailability::NotApplicable {
        Vec::new()
    } else {
        vec![
            t!("view.mssql_agent_jobs.limit.scope"),
            t!("view.mssql_agent_jobs.limit.history"),
            t!("view.mssql_agent_jobs.limit.local_clock"),
            t!("view.mssql_agent_jobs.limit.definition"),
            t!("view.mssql_agent_jobs.limit.owner"),
        ]
    };
    if read.metadata.availability != MssqlAgentAvailability::NotApplicable
        && read.metadata.visibility != MssqlAgentVisibility::Sysadmin
    {
        limitations.push(t!("view.mssql_agent_jobs.limit.visibility"));
    }
    if read.metadata.recheck != MssqlAgentMetadataRecheck::Unchanged
        && read.metadata.availability != MssqlAgentAvailability::NotApplicable
    {
        limitations.push(t!("view.mssql_agent_jobs.limit.recheck"));
    }
    if !read_complete {
        limitations.push(t!("view.jobs.limit.read"));
    }
    if rows.len() > limit {
        limitations.push(t!("view.jobs.limit.presentation"));
    }
    Ok(JobsReport {
        context: read.context.clone(),
        sources,
        read_complete,
        native_rows: read.sources.iter().map(|source| source.records).sum(),
        observed_rows: read.observations.len() as u32,
        entries_limited: rows.len() > limit,
        title: t!("view.mssql_agent_jobs.title"),
        summary: if read.metadata.availability == MssqlAgentAvailability::NotApplicable {
            t!("view.mssql_agent_jobs.summary.not_applicable")
        } else if read.observations.is_empty() {
            t!("view.mssql_agent_jobs.summary.unavailable")
        } else {
            t!("view.mssql_agent_jobs.summary.rows", count = format::digits(read.observations.len().to_string()))
        },
        limitations,
        entries,
    })
}
fn priority(fact: &OperationalFact) -> u8 {
    match fact {
        OperationalFact::MssqlAgentHistory(point) if point.run_status == 0 => 0,
        OperationalFact::MssqlAgentHistory(_) => 1,
        OperationalFact::MssqlAgentJob(_) => 2,
        OperationalFact::MssqlAgentStep(_) => 3,
        _ => 4,
    }
}
fn entry(row: &OperationalObservation) -> Result<JobEntry> {
    let (job_id, kind, title, explanation, metrics) = match &row.fact {
        OperationalFact::MssqlAgentJob(point) => (
            Some(point.job_id.clone()),
            JobEntryKind::State,
            t!("view.mssql_agent_jobs.job.title"),
            job_explanation(point),
            job_metrics(point),
        ),
        OperationalFact::MssqlAgentStep(point) => (
            Some(point.job_id.clone()),
            JobEntryKind::State,
            t!("view.mssql_agent_jobs.step.title"),
            t!("view.mssql_agent_jobs.step.explanation"),
            step_metrics(point),
        ),
        OperationalFact::MssqlAgentHistory(point) => (
            Some(point.job_id.clone()),
            JobEntryKind::RetainedRun,
            t!("view.mssql_agent_jobs.history.title"),
            history_explanation(point.run_status),
            history_metrics(point),
        ),
        OperationalFact::MssqlAgentContext(point) => (
            None,
            JobEntryKind::AgentContext,
            t!("view.mssql_agent_jobs.context.title"),
            t!("view.mssql_agent_jobs.context.explanation"),
            context_metrics(point),
        ),
        _ => return Err(invalid()),
    };
    Ok(JobEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        job_id,
        kind,
        title,
        explanation,
        metrics,
        investigation: t!("view.mssql_agent_jobs.investigate"),
        verify: t!("view.mssql_agent_jobs.verify"),
    })
}
fn job_explanation(point: &MssqlAgentJob) -> String {
    let mut text = t!("view.mssql_agent_jobs.job.definition");
    if let Some(activity) = &point.activity {
        text.push(' ');
        text.push_str(&match (&activity.started_without_offset, &activity.stopped_without_offset) {
            (Some(_), None) => t!("view.mssql_agent_jobs.job.unfinished"),
            (_, Some(_)) => t!("view.mssql_agent_jobs.job.stopped"),
            _ => t!("view.mssql_agent_jobs.job.unreported"),
        });
    } else {
        text.push(' ');
        text.push_str(&t!("view.mssql_agent_jobs.job.no_activity"));
    }
    text
}
fn job_metrics(point: &MssqlAgentJob) -> Vec<Metric> {
    let mut values = vec![
        metric(culprit_core::key!("view.mssql_agent_jobs.field.name"), &point.name),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.observed_at"), &point.observed_at),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.job_id"), &point.job_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.enabled"), if point.enabled { "1" } else { "0" }),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.version"), &point.version_number),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.start_step"), &point.start_step_id),
    ];
    optional(
        &mut values,
        culprit_core::key!("view.mssql_agent_jobs.field.created"),
        point.created_without_offset.as_deref(),
    );
    optional(
        &mut values,
        culprit_core::key!("view.mssql_agent_jobs.field.modified"),
        point.modified_without_offset.as_deref(),
    );
    if let Some(activity) = &point.activity {
        values
            .push(metric(culprit_core::key!("view.mssql_agent_jobs.field.agent_session"), &activity.agent_session_id));
        for (key, value) in [
            (culprit_core::key!("view.mssql_agent_jobs.field.requested"), activity.requested_without_offset.as_deref()),
            (culprit_core::key!("view.mssql_agent_jobs.field.request_source"), activity.request_source.as_deref()),
            (culprit_core::key!("view.mssql_agent_jobs.field.queued"), activity.queued_without_offset.as_deref()),
            (culprit_core::key!("view.mssql_agent_jobs.field.started"), activity.started_without_offset.as_deref()),
            (culprit_core::key!("view.mssql_agent_jobs.field.stopped"), activity.stopped_without_offset.as_deref()),
            (culprit_core::key!("view.mssql_agent_jobs.field.last_step"), activity.last_executed_step_id.as_deref()),
            (
                culprit_core::key!("view.mssql_agent_jobs.field.last_step_time"),
                activity.last_executed_step_without_offset.as_deref(),
            ),
            (
                culprit_core::key!("view.mssql_agent_jobs.field.next_scheduled"),
                activity.next_scheduled_without_offset.as_deref(),
            ),
        ] {
            optional(&mut values, key, value);
        }
    }
    values
}
fn step_metrics(point: &MssqlAgentStep) -> Vec<Metric> {
    let mut values = vec![
        metric(culprit_core::key!("view.mssql_agent_jobs.field.name"), &point.name),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.observed_at"), &point.observed_at),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.job_id"), &point.job_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.step_id"), &point.step_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.subsystem"), &point.subsystem),
    ];
    optional(&mut values, culprit_core::key!("view.mssql_agent_jobs.field.database"), point.database_name.as_deref());
    values
}
fn history_explanation(status: i32) -> String {
    match status {
        0 => t!("view.mssql_agent_jobs.history.failed"),
        1 => t!("view.mssql_agent_jobs.history.succeeded"),
        2 => t!("view.mssql_agent_jobs.history.retry"),
        3 => t!("view.mssql_agent_jobs.history.cancelled"),
        4 => t!("view.mssql_agent_jobs.history.in_progress"),
        _ => t!("view.mssql_agent_jobs.history.unknown"),
    }
}
fn history_metrics(point: &MssqlAgentHistory) -> Vec<Metric> {
    let mut values = vec![
        metric(culprit_core::key!("view.mssql_agent_jobs.field.observed_at"), &point.observed_at),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.job_id"), &point.job_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.history_id"), &point.instance_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.step_id"), &point.step_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.outcome"), &point.run_status.to_string()),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.run_date"), &point.run_date),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.run_time"), &point.run_time),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.duration"), &point.run_duration),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.sql_message"), &point.sql_message_id),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.sql_severity"), &point.sql_severity),
        metric(culprit_core::key!("view.mssql_agent_jobs.field.retries"), &point.retries_attempted),
    ];
    optional(
        &mut values,
        culprit_core::key!("view.mssql_agent_jobs.field.stored_step"),
        point.stored_step_name.as_deref(),
    );
    optional(
        &mut values,
        culprit_core::key!("view.mssql_agent_jobs.field.reported_server"),
        point.reported_server.as_deref(),
    );
    values
}
fn context_metrics(point: &MssqlAgentContext) -> Vec<Metric> {
    let mut values = vec![
        metric(culprit_core::key!("view.mssql_agent_jobs.field.observed_at"), &point.observed_at),
        metric(
            culprit_core::key!("view.mssql_agent_jobs.field.bound"),
            &match point.bound {
                MssqlAgentContextBound::Before => t!("view.mssql_agent_jobs.before"),
                MssqlAgentContextBound::After => t!("view.mssql_agent_jobs.after"),
            },
        ),
    ];
    if let Some(session) = &point.session {
        values.push(metric(culprit_core::key!("view.mssql_agent_jobs.field.agent_session"), &session.session_id));
        optional(
            &mut values,
            culprit_core::key!("view.mssql_agent_jobs.field.agent_start"),
            session.started_without_offset.as_deref(),
        );
    }
    for service in &point.services {
        values.push(metric(culprit_core::key!("view.mssql_agent_jobs.field.service_name"), &service.name));
        values.push(metric(
            culprit_core::key!("view.mssql_agent_jobs.field.service_status"),
            &service.status.map(|value| value.to_string()).unwrap_or_else(|| t!("view.mssql_agent_jobs.unknown")),
        ));
        optional(
            &mut values,
            culprit_core::key!("view.mssql_agent_jobs.field.service_start"),
            service.started_with_offset.as_deref(),
        );
    }
    if point.services_limited {
        values.push(metric(
            culprit_core::key!("view.mssql_agent_jobs.field.service_limit"),
            &t!("view.mssql_agent_jobs.limited"),
        ));
    }
    values
}
fn metric(key: &str, value: &str) -> Metric {
    Metric::said_text(key, value)
}
fn optional(values: &mut Vec<Metric>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        values.push(metric(key, value));
    }
}
fn invalid() -> Error {
    Error::invalid(t!("view.jobs.invalid"))
}
