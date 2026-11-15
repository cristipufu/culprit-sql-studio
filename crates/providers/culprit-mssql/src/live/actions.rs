//! Native session checks and KILL share one held owner. The final conditional guard narrows the race; SQL Server
//! still accepts a numeric SPID, not an atomic session-generation comparison. Never retry a dispatched KILL.
use crate::{
    activity::{ActivityContext, ActivityReader, ActivityRecord, database_context},
    conn::Pool,
    local_source::Context,
    read::{
        NativeRead, Parameter, ReadError, ReadSession, boolean, integer, optional_text, stops_remaining_reads, text,
    },
};
use culprit_core::{
    ErrorCode, Result, SessionAction, SessionActionOutcome, SessionActionTarget, session_number,
    telemetry::{
        ActivityKey, CollectionBudget, CollectionStop, NativeActivityIdentity, NativeReadContext, SourceShutdown,
    },
};

const ACTION_QUERIES: u32 = 8;
const ACTION_BYTES: u32 = 128 * 1024;
const ACTION_STATEMENT_TIMEOUT_MS: u32 = 2_000;
const ACTION_TOTAL_TIMEOUT_MS: u32 = 10_000;
const TARGET_SQL: &str = r#"SELECT TOP (2) /* culprit */ session_id,
 CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS session_generation,
 CAST(is_user_process AS bit) AS user_process,program_name,
 CAST(CASE WHEN session_id=@@SPID THEN 1 ELSE 0 END AS bit) AS own
FROM sys.dm_exec_sessions WHERE session_id=@P1 OPTION(MAXDOP 1)"#;

pub(super) fn optional_authority(result: Result<Context>) -> Result<Option<Context>> {
    match result {
        Ok(context) => Ok(Some(context)),
        Err(error) if stops_remaining_reads(&error) => Err(error),
        Err(error)
            if matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
                || matches!(
                    error.detail,
                    Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceUnavailable))
                ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn complete_authority<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    before: Option<&Context>,
    frame: &ActivityContext,
) -> Result<Option<NativeReadContext>> {
    let Some(before) = before else { return Ok(None) };
    let Some(after) = optional_authority(database_context::recheck(reader, before).await)? else {
        return Ok(None);
    };
    if !before.matches_frame(frame) || !after.matches_frame(frame) || before.connection_changed(&after) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let context = before.completed(&after)?;
    Ok((before.is_qualified() && after.is_qualified()).then_some(context))
}

pub(super) fn target(record: &ActivityRecord, context: Option<&NativeReadContext>) -> Option<SessionActionTarget> {
    let context = context?;
    let database = context.target.database()?;
    if record.facts.database.as_deref() != Some(database.database.as_str())
        || record
            .facts
            .client
            .as_ref()
            .and_then(|client| client.application.as_deref())
            .is_some_and(|application| application.eq_ignore_ascii_case("culprit"))
    {
        return None;
    }
    let target = SessionActionTarget {
        context: context.clone(),
        activity: record.facts.key.clone(),
        statement: record.facts.statement_identity.clone(),
    };
    target.is_valid_for(SessionAction::Terminate).then_some(target)
}

pub(crate) async fn interrupt(
    pool: &std::sync::Arc<Pool>,
    host: &str,
    port: u16,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    validate(target, action)?;
    let budget = CollectionBudget {
        max_queries: ACTION_QUERIES,
        page_bytes: ACTION_BYTES,
        total_bytes: ACTION_BYTES,
        statement_timeout_ms: ACTION_STATEMENT_TIMEOUT_MS,
        total_timeout_ms: ACTION_TOTAL_TIMEOUT_MS,
        ..Default::default()
    };
    budget.validate()?;
    let mut reader = ReadSession::new(pool.clone(), budget);
    interrupt_from(&mut reader, host, port, target, action).await
}

async fn interrupt_from<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    let result = signal(reader, host, port, target, action).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

fn validate(target: &SessionActionTarget, action: SessionAction) -> Result<()> {
    if action == SessionAction::Cancel {
        return Err(super::cancel_unsupported());
    }
    // The guarded native batch compares SQL Server's observed updateability; it cannot use an unknown value.
    if !target.is_valid_for(action) || target.context.database_writable.is_none() {
        return Err(SessionActionTarget::unavailable());
    }
    let id = session_number(&target.activity.session.id)?;
    if !(1..=i16::MAX as u64).contains(&id) || id.to_string() != target.activity.session.id {
        return Err(SessionActionTarget::unavailable());
    }
    crate::sql_time::native_time(&target.activity.session.generation)?;
    Ok(())
}

async fn signal<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    validate(target, action)?;
    let id = session_number(&target.activity.session.id)?;
    let before = action_context(Context::read(reader, host, port).await)?;
    require_source(target, before.completed(&before)?, action)?;
    let current = current_session(reader, id).await?;
    target.require_current(
        &SessionActionTarget { context: before.completed(&before)?, activity: current.clone(), statement: None },
        action,
    )?;
    let after = action_context(before.recheck(reader, host, port).await)?;
    let context = action_context(before.completed(&after))?;
    target.require_current(&SessionActionTarget { context, activity: current, statement: None }, action)?;
    let database = target.context.target.database().ok_or_else(SessionActionTarget::unavailable)?;
    let writable = target.context.database_writable.ok_or_else(SessionActionTarget::unavailable)?;
    let rows = reader
        .query(
            &guarded_kill(id),
            vec![
                Parameter::Integer(i64::try_from(id).map_err(|_| SessionActionTarget::unavailable())?),
                Parameter::Text(target.activity.session.generation.clone()),
                Parameter::Text(database.database.clone()),
                Parameter::Integer(if writable { 1 } else { 0 }),
            ],
            1,
        )
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(SessionActionTarget::signal_failed()) };
    if !boolean(row, "signalled")? {
        return Err(SessionActionTarget::changed());
    }
    Ok(SessionActionOutcome::SignalSent)
}

fn require_source(target: &SessionActionTarget, context: NativeReadContext, action: SessionAction) -> Result<()> {
    if !context.has_qualified_local_owner() {
        return Err(SessionActionTarget::unavailable());
    }
    target.require_current(
        &SessionActionTarget { context, activity: target.activity.clone(), statement: target.statement.clone() },
        action,
    )
}

fn action_context<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if CollectionStop::from_error(&error) == CollectionStop::SourceChanged {
            SessionActionTarget::changed()
        } else {
            error
        }
    })
}

async fn current_session<R: NativeRead>(reader: &mut R, id: u64) -> Result<ActivityKey> {
    let rows = reader
        .query(
            TARGET_SQL,
            vec![Parameter::Integer(i64::try_from(id).map_err(|_| SessionActionTarget::unavailable())?)],
            2,
        )
        .await
        .map_err(ReadError::into_error)?;
    let row = match rows.as_slice() {
        [] => return Err(super::session_gone(id)),
        [row] => row,
        _ => return Err(SessionActionTarget::changed()),
    };
    if boolean(row, "own")?
        || optional_text(row, "program_name")?.is_some_and(|program| program.eq_ignore_ascii_case("culprit"))
    {
        return Err(super::own_session(id));
    }
    if !boolean(row, "user_process")? {
        return Err(super::system_session(id));
    }
    let current = ActivityKey {
        session: NativeActivityIdentity {
            id: integer(row, "session_id")?.to_string(),
            generation: crate::sql_time::native_time(&text(row, "session_generation")?)?,
        },
        request: None,
        transaction: None,
    };
    if current.session.id != id.to_string() || !current.is_valid() {
        return Err(SessionActionTarget::changed());
    }
    Ok(current)
}

fn guarded_kill(id: u64) -> String {
    // KILL cannot bind its SPID argument. Only the parsed integer enters this fixed administrative template.
    // Even in one batch, the predicate and KILL are not an atomic compare-and-signal primitive.
    format!(
        r#"DECLARE /* culprit */ @signalled bit=0;
IF DB_NAME()=@P3
 AND CONVERT(nvarchar(32),DATABASEPROPERTYEX(DB_NAME(),'Updateability'))=CASE @P4 WHEN 1 THEN N'READ_WRITE' ELSE N'READ_ONLY' END
 AND EXISTS(SELECT 1 FROM sys.dm_exec_sessions
   WHERE session_id=@P1 AND login_time=CONVERT(datetime,@P2,126)
    AND is_user_process=1 AND session_id<>@@SPID AND LOWER(COALESCE(program_name,N''))<>N'culprit')
BEGIN
 KILL {id};
 SET @signalled=1;
END;
SELECT @signalled AS signalled;"#
    )
}

#[cfg(test)]
mod tests;
