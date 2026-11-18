//! Native scalar decoding only; commands/messages and historical-to-current definition joins are omitted.
use super::*;
use crate::read::{boolean, optional_counter, optional_signed_integer, signed_integer};

pub(super) fn job(row: &Rec) -> Result<OperationalFact> {
    let session = optional_counter(row, "activity_session_id")?;
    let activity = match session {
        Some(agent_session_id) => Some(MssqlAgentJobActivity {
            agent_session_id,
            requested_without_offset: optional_text(row, "requested_without_offset")?,
            request_source: optional_text(row, "request_source")?,
            queued_without_offset: optional_text(row, "queued_without_offset")?,
            started_without_offset: optional_text(row, "started_without_offset")?,
            stopped_without_offset: optional_text(row, "stopped_without_offset")?,
            last_executed_step_id: optional_counter(row, "last_executed_step_id")?,
            last_executed_step_without_offset: optional_text(row, "last_step_without_offset")?,
            next_scheduled_without_offset: optional_text(row, "next_scheduled_without_offset")?,
        }),
        None => {
            for name in [
                "requested_without_offset",
                "request_source",
                "queued_without_offset",
                "started_without_offset",
                "stopped_without_offset",
                "last_step_without_offset",
                "next_scheduled_without_offset",
            ] {
                if optional_text(row, name)?.is_some() {
                    return Err(invalid_data());
                }
            }
            if optional_counter(row, "last_executed_step_id")?.is_some() {
                return Err(invalid_data());
            }
            None
        }
    };
    let point = MssqlAgentJob {
        observed_at: at(row)?,
        job_id: text(row, "job_id")?,
        name: text(row, "name")?,
        enabled: boolean(row, "enabled")?,
        start_step_id: count(row, "start_step_id")?,
        version_number: count(row, "version_number")?,
        created_without_offset: optional_text(row, "created_without_offset")?,
        modified_without_offset: optional_text(row, "modified_without_offset")?,
        activity,
    };
    valid(OperationalFact::MssqlAgentJob(Box::new(point)))
}
pub(super) fn step(row: &Rec) -> Result<OperationalFact> {
    valid(OperationalFact::MssqlAgentStep(Box::new(MssqlAgentStep {
        observed_at: at(row)?,
        job_id: text(row, "job_id")?,
        step_id: count(row, "step_id")?,
        name: text(row, "step_name")?,
        subsystem: text(row, "subsystem")?,
        database_name: optional_text(row, "database_name")?,
    })))
}
pub(super) fn history(row: &Rec) -> Result<OperationalFact> {
    valid(OperationalFact::MssqlAgentHistory(Box::new(MssqlAgentHistory {
        observed_at: at(row)?,
        job_id: text(row, "job_id")?,
        instance_id: count(row, "instance_id")?,
        step_id: count(row, "step_id")?,
        stored_step_name: optional_text(row, "step_name")?,
        run_status: i32::try_from(signed_integer(row, "run_status")?).map_err(|_| invalid_data())?,
        run_date: count(row, "run_date")?,
        run_time: count(row, "run_time")?,
        run_duration: count(row, "run_duration")?,
        sql_message_id: signed_code(row, "sql_message_id")?,
        sql_severity: signed_code(row, "sql_severity")?,
        retries_attempted: count(row, "retries_attempted")?,
        reported_server: optional_text(row, "server")?,
    })))
}
pub(super) fn context(rows: &[Rec], bound: MssqlAgentContextBound) -> Result<MssqlAgentContext> {
    let first = rows.first().ok_or_else(invalid_data)?;
    if rows.len() > MAX_MSSQL_AGENT_SERVICES {
        return Err(CollectionStop::RowLimit.error());
    }
    let observed_at = at(first)?;
    let observed_session = session(first)?;
    let mut services = Vec::new();
    for row in rows {
        if at(row)? != observed_at || session(row)? != observed_session {
            return Err(invalid_data());
        }
        if let Some(name) = optional_text(row, "servicename")? {
            services.push(MssqlAgentService {
                name,
                status: optional_signed_integer(row, "service_status")?
                    .map(i32::try_from)
                    .transpose()
                    .map_err(|_| invalid_data())?,
                started_with_offset: optional_text(row, "started_with_offset")?,
            });
        } else if optional_signed_integer(row, "service_status")?.is_some()
            || optional_text(row, "started_with_offset")?.is_some()
        {
            return Err(invalid_data());
        }
    }
    let point = MssqlAgentContext {
        observed_at,
        bound,
        session: observed_session,
        services_limited: services.len() == MAX_MSSQL_AGENT_SERVICES,
        services,
    };
    point.is_valid().then_some(point).ok_or_else(invalid_data)
}
fn session(row: &Rec) -> Result<Option<MssqlAgentSession>> {
    let id = optional_counter(row, "session_id")?;
    let started = optional_text(row, "agent_started_without_offset")?;
    match (id, started) {
        (None, None) => Ok(None),
        (Some(session_id), started_without_offset) => {
            Ok(Some(MssqlAgentSession { session_id, started_without_offset }))
        }
        _ => Err(invalid_data()),
    }
}
fn count(row: &Rec, name: &str) -> Result<String> {
    i32::try_from(integer(row, name)?).map(|value| value.to_string()).map_err(|_| invalid_data())
}
fn at(row: &Rec) -> Result<String> {
    canonical_time(&text(row, "observed_at")?)
}
fn valid(fact: OperationalFact) -> Result<OperationalFact> {
    fact.is_valid().then_some(fact).ok_or_else(invalid_data)
}

fn signed_code(row: &Rec, name: &str) -> Result<String> {
    i32::try_from(signed_integer(row, name)?).map(|value| value.to_string()).map_err(|_| invalid_data())
}
