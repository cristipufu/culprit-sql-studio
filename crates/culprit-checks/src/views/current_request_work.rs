//! Native event counters describe the selected request's work, without deriving spill cost or resource limits.
use culprit_core::{
    CurrentRequestWorkEntry, CurrentRequestWorkReport, Error, Metric, Result, format, key, t, telemetry::*,
};

mod postgres;

pub fn explain(snapshot: &CurrentRequestWorkSnapshot, read_complete: bool) -> Result<CurrentRequestWorkReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(Error::invalid(t!("view.current_request_work.invalid")));
    }
    let mut limitations = vec![t!("view.current_request_work.limit.point")];
    if snapshot.profile == CurrentRequestWorkProfile::MysqlStatementEvent {
        limitations
            .extend([t!("view.current_request_work.limit.temporary"), t!("view.current_request_work.limit.timers")]);
    }
    limitations.push(t!("view.current_request_work.limit.disappearance"));
    if snapshot.profile == CurrentRequestWorkProfile::PostgresCopy {
        limitations.extend(postgres::limitations());
    }
    if !snapshot.context.has_qualified_local_owner() {
        limitations.push(t!("view.current_request_work.limit.current_scope"));
    }
    if !read_complete || snapshot.observations.len() != snapshot.coverage.records as usize {
        limitations.push(t!("view.current_request_work.limit.read"));
    }
    if !snapshot.coverage.complete_scope {
        limitations.push(t!("view.current_request_work.limit.sampled"));
    }
    if snapshot
        .observations
        .iter()
        .any(|row| row.scope == ActivityScopeBasis::SessionDatabase && row.native.mysql().is_some())
    {
        limitations.push(t!("view.current_request_work.limit.session_database"));
    }
    if snapshot
        .observations
        .iter()
        .any(|row| row.native.mysql().is_some_and(|work| work.rows_examined.as_deref() == Some("0")))
    {
        limitations.push(t!("view.current_request_work.limit.rows_examined"));
    }
    if snapshot.observations.iter().any(|row| matches!(row.statement, DefinitionPayload::Unavailable { .. })) {
        limitations.push(t!("view.current_request_work.limit.statement"));
    }
    if snapshot
        .observations
        .iter()
        .any(|row| row.native.mysql().is_some_and(|work| matches!(work.stage, MysqlStageObservation::Observed { .. })))
    {
        limitations.push(t!("view.current_request_work.limit.stage"));
    }
    Ok(CurrentRequestWorkReport {
        source: snapshot.source.clone(),
        context: snapshot.context.clone(),
        source_state: snapshot.coverage.state,
        source_reason: None,
        read_complete,
        native_rows: snapshot.coverage.records,
        observed_rows: snapshot.observations.len() as u32,
        binding: snapshot.result.binding,
        membership: snapshot.result.membership,
        title: t!("view.current_request_work.title"),
        summary: summary(snapshot),
        limitations,
        entries: snapshot.observations.iter().map(entry).collect::<Result<_>>()?,
    })
}

fn summary(snapshot: &CurrentRequestWorkSnapshot) -> String {
    match snapshot.result.binding {
        FocusedRequestBinding::ParticipantMissing => t!("view.current_request_work.summary.missing"),
        FocusedRequestBinding::ParticipantChanged => t!("view.current_request_work.summary.changed"),
        _ => match snapshot.result.membership {
            FocusedRequestMembership::Unavailable => t!("view.current_request_work.summary.unavailable"),
            FocusedRequestMembership::NotObserved => match snapshot.profile {
                CurrentRequestWorkProfile::MysqlStatementEvent => t!("view.current_request_work.summary.not_observed"),
                CurrentRequestWorkProfile::PostgresCopy => t!("view.current_request_work.copy.summary.not_observed"),
            },
            FocusedRequestMembership::Observed if snapshot.observations.is_empty() => {
                t!("view.current_request_work.summary.read_gap")
            }
            FocusedRequestMembership::Observed => match snapshot.result.binding {
                FocusedRequestBinding::Matched => t!("view.current_request_work.summary.matched"),
                FocusedRequestBinding::UnverifiedOriginalScope => t!("view.current_request_work.summary.unverified"),
                _ => t!("view.current_request_work.summary.independent"),
            },
        },
    }
}

fn entry(row: &CurrentRequestWorkObservation) -> Result<CurrentRequestWorkEntry> {
    let details = match &row.native {
        CurrentRequestWorkNative::Mysql { work } => mysql_details(work),
        CurrentRequestWorkNative::PostgresCopy { progress } => postgres::details(progress),
    };
    Ok(CurrentRequestWorkEntry {
        observation_key: row.key()?,
        observed_at: row.observed_at.clone(),
        actor: row.actor.clone(),
        database: row.database.clone(),
        scope: row.scope,
        statement_identity: row.statement_identity.clone(),
        statement: row.statement.clone(),
        title: t!("view.current_request_work.entry.title", session = &row.actor.session.id),
        explanation: details.explanation,
        metrics: details.metrics,
        investigation: details.investigation,
        verify: details.verify,
    })
}

struct Details {
    explanation: String,
    metrics: Vec<Metric>,
    investigation: String,
    verify: String,
}

fn mysql_details(native: &MysqlCurrentRequestWork) -> Details {
    let mut explanation = vec![explain_counters(native), explain_stage(&native.stage)];
    if matches!(&native.stage, MysqlStageObservation::Observed { event } if event.timer_wait_ps.is_none()) {
        explanation.push(t!("view.current_request_work.stage.untimed"));
    }
    Details {
        explanation: explanation.join(" "),
        metrics: metrics(native),
        investigation: t!("view.current_request_work.investigate"),
        verify: t!("view.current_request_work.verify"),
    }
}

fn explain_counters(native: &MysqlCurrentRequestWork) -> String {
    let temporary = [&native.created_tmp_tables, &native.created_tmp_disk_tables]
        .into_iter()
        .any(|value| value.as_deref().is_some_and(|value| value != "0"));
    let sort = native.sort_merge_passes.as_deref().is_some_and(|value| value != "0");
    match (temporary, sort) {
        (true, true) => t!("view.current_request_work.entry.mixed"),
        (true, false) => t!("view.current_request_work.entry.temporary"),
        (false, true) => t!("view.current_request_work.entry.sort"),
        (false, false) => t!("view.current_request_work.entry.observed"),
    }
}

fn explain_stage(stage: &MysqlStageObservation) -> String {
    match stage {
        MysqlStageObservation::Observed { .. } => t!("view.current_request_work.stage.observed"),
        MysqlStageObservation::NotObserved => t!("view.current_request_work.stage.not_observed"),
        MysqlStageObservation::Unlinked => t!("view.current_request_work.stage.unlinked"),
        MysqlStageObservation::Unavailable { reason } => match reason {
            MysqlStageUnavailable::InstrumentationDisabled => t!("view.current_request_work.stage.disabled"),
            MysqlStageUnavailable::PermissionDenied => t!("view.current_request_work.stage.denied"),
            MysqlStageUnavailable::Unsupported => t!("view.current_request_work.stage.unsupported"),
            MysqlStageUnavailable::NotCollected => t!("view.current_request_work.stage.not_collected"),
        },
    }
}

fn metrics(native: &MysqlCurrentRequestWork) -> Vec<Metric> {
    let mut metrics = vec![
        Metric::said_text(key!("view.current_request_work.metric.thread_id"), format::digits(native.thread_id.clone())),
        Metric::said_text(
            key!("view.current_request_work.metric.statement_event_id"),
            format::digits(native.statement_event_id.clone()),
        ),
        Metric::said_text(
            key!("view.current_request_work.metric.timer_start_ps"),
            format::digits(native.timer_start_ps.clone()),
        ),
    ];
    metrics.extend(counter_metrics([
        (key!("view.current_request_work.metric.timer_wait_ps"), &native.timer_wait_ps),
        (key!("view.current_request_work.metric.rows_examined"), &native.rows_examined),
        (key!("view.current_request_work.metric.rows_sent"), &native.rows_sent),
        (key!("view.current_request_work.metric.created_tmp_tables"), &native.created_tmp_tables),
        (key!("view.current_request_work.metric.created_tmp_disk_tables"), &native.created_tmp_disk_tables),
        (key!("view.current_request_work.metric.sort_merge_passes"), &native.sort_merge_passes),
    ]));
    if let MysqlStageObservation::Observed { event } = &native.stage {
        metrics.extend([
            Metric::said_text(key!("view.current_request_work.metric.stage_name"), &event.event_name),
            Metric::said_text(
                key!("view.current_request_work.metric.stage_event_id"),
                format::digits(event.event_id.clone()),
            ),
            Metric::said_text(
                key!("view.current_request_work.metric.stage_parent_event_id"),
                format::digits(event.nesting_event_id.clone()),
            ),
        ]);
        metrics.extend(counter_metrics([
            (key!("view.current_request_work.metric.stage_timer_start_ps"), &event.timer_start_ps),
            (key!("view.current_request_work.metric.stage_timer_wait_ps"), &event.timer_wait_ps),
            (key!("view.current_request_work.metric.stage_work_completed"), &event.work_completed),
            (key!("view.current_request_work.metric.stage_work_estimated"), &event.work_estimated),
        ]));
    }
    metrics
}

fn counter_metrics<'a>(fields: impl IntoIterator<Item = (&'a str, &'a Option<String>)>) -> Vec<Metric> {
    fields
        .into_iter()
        .filter_map(|(label, value)| {
            value.as_ref().map(|value| Metric::said_text(label, format::digits(value.clone())))
        })
        .collect()
}

#[cfg(test)]
mod tests;
