//! Native facts are decoded once. Unredacted statement text stays private until a consumer projects it.
use super::{ActivityRecord, NativeStatement, PreviousBatchLocator, RequestLocator, resources};
use crate::{
    read::{boolean, integer, invalid_data, optional_counter, optional_text, text},
    sql_time::native_time,
    value::{Rec, Val},
};
use culprit_core::{Result, telemetry::*};

pub(super) fn value<'a>(row: &'a Rec, name: &str) -> Result<&'a Val> {
    row.columns().find(|(column, _)| *column == name).map(|(_, value)| value).ok_or_else(invalid_data)
}

fn generation(row: &Rec, name: &str) -> Result<String> {
    native_time(&text(row, name)?)
}
fn age(row: &Rec, start: &str) -> Result<Option<String>> {
    let Some(start) = optional_text(row, start)? else { return Ok(None) };
    let parse =
        |value: &str| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").map_err(|_| invalid_data());
    let now = parse(&text(row, "native_observed_at")?)?;
    let start = parse(&start)?;
    Ok((start <= now).then(|| (now - start).num_milliseconds().to_string()))
}
fn counters(row: &Rec, prefix: &str) -> Result<ActivityCounters> {
    Ok(ActivityCounters {
        cpu_ms: optional_counter(row, &format!("{prefix}_cpu_ms"))?,
        logical_reads: optional_counter(row, &format!("{prefix}_logical_reads"))?,
        physical_reads: optional_counter(row, &format!("{prefix}_physical_reads"))?,
        writes: optional_counter(row, &format!("{prefix}_writes"))?,
    })
}
pub(super) fn record(row: &Rec) -> Result<Option<ActivityRecord>> {
    let Some(session) = optional_counter(row, "session_id")? else { return Ok(None) };
    let request = optional_counter(row, "request_id")?
        .map(|id| {
            Ok::<_, culprit_core::Error>(NativeActivityIdentity {
                id,
                generation: generation(row, "request_generation")?,
            })
        })
        .transpose()?;
    let no_current_request = boolean(row, "no_current_request")?;
    // DMV reads are not atomic: a request observed in the join can end before the separate absence probe.
    // Preserve that positive observation; the SQL enrichment guard will refuse a statement that has ended.
    let idle = request.is_none() && no_current_request;
    let native_type = optional_text(row, "wait_type")?;
    let waiting_on_lock = native_type.as_deref().filter(|wait| crate::waits::is_lock_wait_type(wait)).map(|_| true);
    let owner = owner(row)?;
    let wait = if native_type.is_some() || owner.is_some() {
        Some(ActivityWait {
            identified_resources: resources::identify(
                native_type.as_deref(),
                optional_text(row, "wait_resource")?.as_deref(),
            ),
            resources: None,
            native_type,
            elapsed_ms: optional_counter(row, "wait_ms")?,
            owners: owner.into_iter().collect(),
        })
    } else {
        None
    };
    let current_locator = request.as_ref().map(|_| request_locator(row)).transpose()?.flatten();
    let observation = ActivityObservation {
        waiting_on_lock,
        blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
        key: ActivityKey {
            session: NativeActivityIdentity { id: session, generation: generation(row, "session_generation")? },
            transaction: if request.is_none() { optional_counter(row, "transaction_id")? } else { None },
            request: request.clone(),
        },
        idle,
        scope: match integer(row, "scope_basis")? {
            1 => ActivityScopeBasis::RequestDatabase,
            2 => ActivityScopeBasis::TransactionDatabase,
            3 => ActivityScopeBasis::SessionDatabase,
            4 => ActivityScopeBasis::BlockingReference,
            5 => ActivityScopeBasis::OtherDatabase,
            _ => return Err(invalid_data()),
        },
        state: text(row, "state")?,
        database: optional_text(row, "session_database")?,
        command: optional_text(row, "command")?,
        last_request_age_ms: age(row, "last_request_start")?,
        open_transactions: optional_counter(row, "open_transaction_count")?
            .map(|value| value.parse::<u32>().map_err(|_| invalid_data()))
            .transpose()?,
        elapsed_ms: optional_counter(row, "elapsed_ms")?,
        transaction_age_ms: age(row, "transaction_start")?,
        session_counters: counters(row, "session")?,
        request_counters: if request.is_some() { Some(counters(row, "request")?) } else { None },
        wait,
        query_hash: optional_text(row, "query_hash")?,
        statement_identity: current_locator.as_ref().map(RequestLocator::identity),
        client: Some(ActivityClient {
            application: optional_text(row, "program_name")?,
            user: optional_text(row, "login_name")?,
            host: optional_text(row, "host_name")?,
        }),
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    };
    if !observation.is_valid() {
        return Err(invalid_data());
    }
    let previous_locator = if observation.idle && request.is_none() { previous_locator(row)? } else { None };
    Ok(Some(ActivityRecord {
        facts: observation,
        current_sql: NativeStatement::Unavailable(if request.is_none() || current_locator.is_some() {
            PayloadUnavailable::NotCollected
        } else {
            PayloadUnavailable::Missing
        }),
        previous_sql: NativeStatement::Unavailable(PayloadUnavailable::NotCollected),
        current_locator,
        previous_locator,
    }))
}
fn request_locator(row: &Rec) -> Result<Option<RequestLocator>> {
    let Some(handle) = optional_text(row, "sql_handle")? else { return Ok(None) };
    // Native work without an active statement has no SQL locator, even when its request and counters are visible.
    let start = match value(row, "statement_start")? {
        Val::Null | Val::Int(-1) => return Ok(None),
        Val::Int(value) if *value >= 0 => *value,
        _ => return Err(invalid_data()),
    };
    const MAX_HANDLE_BYTES: usize = 64;
    if handle.is_empty()
        || handle.len() > MAX_HANDLE_BYTES * 2
        || !handle.len().is_multiple_of(2)
        || !handle.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_data());
    }
    let end = match value(row, "statement_end")? {
        Val::Null => return Ok(None),
        Val::Int(value) if *value == -1 || *value >= start => *value,
        _ => return Err(invalid_data()),
    };
    Ok(RequestLocator::from_native(
        handle,
        i32::try_from(start).map_err(|_| invalid_data())?,
        i32::try_from(end).map_err(|_| invalid_data())?,
        integer(row, "request_database_id")?.try_into().map_err(|_| invalid_data())?,
    ))
}
fn previous_locator(row: &Rec) -> Result<Option<PreviousBatchLocator>> {
    match (optional_text(row, "last_request_start")?, optional_text(row, "last_request_end")?) {
        (Some(start), Some(end)) => {
            Ok(Some(PreviousBatchLocator { start: native_time(&start)?, end: native_time(&end)? }))
        }
        _ => Ok(None),
    }
}
fn owner(row: &Rec) -> Result<Option<ActivityWaitOwner>> {
    let id = match value(row, "blocking_session_id")? {
        Val::Null => return Ok(None),
        Val::Int(value) => *value,
        _ => return Err(invalid_data()),
    };
    Ok(match id {
        0 => None,
        id if id > 0 => Some(ActivityWaitOwner::Session {
            id: id.to_string(),
            generation: optional_text(row, "blocker_generation")?.map(|value| native_time(&value)).transpose()?,
        }),
        -2 => Some(ActivityWaitOwner::Transaction { native_id: id.to_string(), owner: TransactionWaitOwner::Orphaned }),
        -3 => Some(ActivityWaitOwner::Transaction { native_id: id.to_string(), owner: TransactionWaitOwner::Recovery }),
        -4 => Some(ActivityWaitOwner::Unknown { native_id: id.to_string(), reason: UnknownWaitOwner::LatchTransition }),
        -5 => Some(ActivityWaitOwner::Unknown { native_id: id.to_string(), reason: UnknownWaitOwner::UntrackedLatch }),
        _ => Some(ActivityWaitOwner::Unknown { native_id: id.to_string(), reason: UnknownWaitOwner::Other }),
    })
}
pub(super) fn detail_key(row: &Rec, previous: bool) -> Result<(String, Option<String>)> {
    Ok((
        integer(row, "session_id")?.to_string(),
        if previous { None } else { Some(integer(row, "request_id")?.to_string()) },
    ))
}
pub(super) fn statement(row: &Rec, record: &ActivityRecord, cap: u32, previous: bool) -> Result<NativeStatement> {
    if integer(row, "session_id")?.to_string() != record.facts.key.session.id
        || generation(row, "session_generation")? != record.facts.key.session.generation
    {
        return Err(invalid_data());
    }
    if !previous {
        let request = record.facts.key.request.as_ref().ok_or_else(invalid_data)?;
        if integer(row, "request_id")?.to_string() != request.id
            || generation(row, "request_generation")? != request.generation
        {
            return Err(invalid_data());
        }
    }
    if !boolean(row, "matched")? {
        return Ok(NativeStatement::Unavailable(PayloadUnavailable::Missing));
    }
    let bytes = optional_counter(row, "source_bytes")?
        .map(|value| value.parse::<u64>().map_err(|_| invalid_data()))
        .transpose()?;
    let encrypted = match value(row, "encrypted")? {
        Val::Null => false,
        Val::Bool(value) => *value,
        Val::Int(0) => false,
        Val::Int(1) => true,
        _ => return Err(invalid_data()),
    };
    Ok(if encrypted {
        NativeStatement::Unavailable(PayloadUnavailable::Encrypted)
    } else if bytes.is_some_and(|bytes| bytes > u64::from(cap)) {
        NativeStatement::Unavailable(PayloadUnavailable::Oversized)
    } else if let Some(payload) = optional_text(row, "payload")? {
        if bytes != Some((payload.encode_utf16().count() as u64) * 2) {
            return Err(invalid_data());
        }
        NativeStatement::Available(payload)
    } else {
        NativeStatement::Unavailable(PayloadUnavailable::Missing)
    })
}
