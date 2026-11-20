//! Fixture-only Agent scheduling isolation. A request timestamp is not an execution witness.
use super::{CONTROL_TIMEOUT, Fixture};
use crate::{
    read::{boolean, integer, invalid_data, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, canonical_time},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

const MAX_RUNTIME_OBSERVATIONS: usize = 20;
const RUNTIME_OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);
const RUNTIME_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(2);
// Capture jobs can have multiple steps. Every step must remain in this nonce-owned database.
const OWNED_CAPTURE_JOB: &str = r#"
SELECT c.job_id,j.enabled FROM msdb.dbo.cdc_jobs c JOIN msdb.dbo.sysjobs j ON j.job_id=c.job_id
WHERE c.database_id=DB_ID() AND c.job_type=N'capture' AND j.name=N'cdc.'+DB_NAME()+N'_capture'
AND (SELECT COUNT_BIG(*) FROM msdb.dbo.cdc_jobs WHERE database_id=DB_ID() AND job_type=N'capture')=1
AND EXISTS(SELECT 1 FROM msdb.dbo.sysjobsteps s WHERE s.job_id=j.job_id AND s.subsystem=N'TSQL' AND s.database_name=DB_NAME())
AND NOT EXISTS(SELECT 1 FROM msdb.dbo.sysjobsteps s WHERE s.job_id=j.job_id
 AND (s.subsystem<>N'TSQL' OR s.database_name IS NULL OR s.database_name<>DB_NAME()))
"#;

impl Fixture {
    pub(super) async fn disable_capture_job(&mut self) -> Result<()> {
        Box::pin(async {
            self.control.require_available()?;
            let mut phase = CaptureJobPhase::new(self);
            let result = Box::pin(phase.settle()).await;
            let receipt = serde_json::to_value(&phase.witness).map_err(|_| invalid_data());
            let result = match (result, receipt) {
                (result, Ok(receipt)) => {
                    phase.fixture.capture_job = Some(receipt);
                    result
                }
                (Err(error), Err(_)) | (Ok(()), Err(error)) => Err(error),
            };
            if result.is_ok() {
                phase.complete = true;
            } else {
                phase.fixture.control.fence();
                // Preserve the original failure. Retirement is separately recorded and never reopens admission.
                let _ = Box::pin(phase.fixture.retire()).await;
            }
            result
        })
        .await
    }
}

/// Cancellation between drained commands must not promote an unfinished scheduling phase to safe cleanup.
struct CaptureJobPhase<'a> {
    fixture: &'a mut Fixture,
    witness: CaptureJobWitness,
    complete: bool,
}
impl<'a> CaptureJobPhase<'a> {
    fn new(fixture: &'a mut Fixture) -> Self {
        Self { fixture, witness: CaptureJobWitness::default(), complete: false }
    }
    async fn settle(&mut self) -> Result<()> {
        Box::pin(async {
            self.disable().await?;
            let original = self.witness.disabled.as_ref().ok_or_else(invalid_data)?.clone();
            let mut latest = original.observed_at.clone();
            let mut budget = SettlementBudget::new();
            loop {
                let timeout = budget.next_observation()?;
                let state = self.observe(&original, &latest, timeout).await?;
                budget.remaining()?;
                latest = self.witness.runtime.last().ok_or_else(invalid_data)?.after.observed_at.clone();
                match self.witness.action(state) {
                    JobAction::Complete => {
                        self.witness.settled_idle = true;
                        return Ok(());
                    }
                    JobAction::Stop => {
                        self.stop(&original, &latest, budget.remaining()?).await?;
                        budget.remaining()?;
                        latest = self.witness.stop.as_ref().ok_or_else(invalid_data)?.after.observed_at.clone();
                    }
                    JobAction::Observe => {}
                }
                budget.pause().await?;
            }
        })
        .await
    }
    async fn disable(&mut self) -> Result<()> {
        let observed = &mut self.witness.disabled;
        self.fixture
            .query_checked("disable-owned-capture-job", CONTROL_TIMEOUT, &disable_sql(), |rows| {
                let [row] = rows.as_slice() else { return Err(invalid_data()) };
                let guard = JobGuard::read(row, "disabled")?;
                let valid = guard.require_qualified();
                *observed = Some(guard);
                valid?;
                if integer(row, "disable_return_code")? != 0 {
                    return Err(invalid_data());
                }
                Ok(())
            })
            .await
    }
    async fn observe(&mut self, original: &JobGuard, latest: &str, timeout: Duration) -> Result<RuntimeState> {
        let observed = &mut self.witness.runtime;
        self.fixture
            .query_checked("observe-owned-capture-job-runtime", timeout, &runtime_sql(&original.owner), |rows| {
                let runtime = Guarded::read(&rows, JobRuntime::read)?;
                let valid =
                    runtime.require_original(original, latest).and_then(|()| runtime.value.state(&original.owner));
                observed.push(runtime);
                valid
            })
            .await
    }
    async fn stop(&mut self, original: &JobGuard, latest: &str, timeout: Duration) -> Result<()> {
        self.witness.stop_requested = true;
        let observed = &mut self.witness.stop;
        self.fixture
            .query_checked("stop-executing-owned-capture-job", timeout, &stop_sql(&original.owner), |rows| {
                let stopped = Guarded::read(&rows, |row| integer(row, "stop_return_code"))?;
                let valid = stopped.require_stop_ack(original, latest);
                *observed = Some(stopped);
                valid
            })
            .await
    }
}
impl Drop for CaptureJobPhase<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.fixture.control.fence();
        }
    }
}

struct SettlementBudget {
    deadline: Instant,
    observations: usize,
}
impl SettlementBudget {
    fn new() -> Self {
        Self { deadline: Instant::now() + RUNTIME_SETTLEMENT_TIMEOUT, observations: 0 }
    }
    fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.min(CONTROL_TIMEOUT))
            .ok_or_else(|| CollectionStop::TimedOut.error())
    }
    fn next_observation(&mut self) -> Result<Duration> {
        if self.observations >= MAX_RUNTIME_OBSERVATIONS {
            return Err(CollectionStop::SourceNotReady.error());
        }
        let remaining = self.remaining()?;
        self.observations += 1;
        Ok(remaining)
    }
    async fn pause(&self) -> Result<()> {
        if self.observations >= MAX_RUNTIME_OBSERVATIONS {
            return Err(CollectionStop::SourceNotReady.error());
        }
        tokio::time::sleep(RUNTIME_OBSERVATION_INTERVAL.min(self.remaining()?)).await;
        self.remaining().map(|_| ())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct JobOwner {
    job_id: String,
    job_name: String,
    database_id: i64,
    agent_session_id: i64,
}
#[derive(Clone, Serialize)]
struct JobGuard {
    owner: JobOwner,
    observed_at: String,
    exact_owned_job: bool,
    same_agent_session: bool,
    agent_running: bool,
    job_disabled: bool,
    activity_row_present: bool,
    run_requested_without_offset: Option<String>,
    queued_without_offset: Option<String>,
    execution_started_without_offset: Option<String>,
    execution_stopped_without_offset: Option<String>,
}
impl JobGuard {
    fn read(row: &Rec, phase: &str) -> Result<Self> {
        if text(row, "witness_phase")? != phase {
            return Err(invalid_data());
        }
        Ok(Self {
            owner: JobOwner {
                job_id: job_id(row)?,
                job_name: text(row, "job_name")?,
                database_id: integer(row, "database_id")?,
                agent_session_id: integer(row, "agent_session_id")?,
            },
            observed_at: canonical_time(&text(row, "observed_at")?)?,
            exact_owned_job: boolean(row, "exact_owned_job")?,
            same_agent_session: boolean(row, "same_agent_session")?,
            agent_running: boolean(row, "agent_running")?,
            job_disabled: boolean(row, "job_disabled")?,
            activity_row_present: boolean(row, "activity_row_present")?,
            run_requested_without_offset: optional_text(row, "run_requested_without_offset")?,
            queued_without_offset: optional_text(row, "queued_without_offset")?,
            execution_started_without_offset: optional_text(row, "execution_started_without_offset")?,
            execution_stopped_without_offset: optional_text(row, "execution_stopped_without_offset")?,
        })
    }
    fn require_qualified(&self) -> Result<()> {
        if self.owner.database_id > 4
            && (1..=i64::from(i32::MAX)).contains(&self.owner.agent_session_id)
            && self.exact_owned_job
            && self.same_agent_session
            && self.agent_running
            && self.job_disabled
        {
            Ok(())
        } else {
            Err(invalid_data())
        }
    }
}
fn job_id(row: &Rec) -> Result<String> {
    // Both SQL CONVERT and native uniqueidentifier values use the existing UUID parser/canonical display.
    let id = tiberius::Uuid::parse_str(&text(row, "job_id")?).map_err(|_| invalid_data())?;
    if id.is_nil() {
        return Err(invalid_data());
    }
    Ok(id.to_string())
}

#[derive(Serialize)]
struct Guarded<T> {
    before: JobGuard,
    value: T,
    after: JobGuard,
}
impl<T> Guarded<T> {
    fn read(rows: &[Rec], read: impl FnOnce(&Rec) -> Result<T>) -> Result<Self> {
        let [before, value, after] = rows else { return Err(invalid_data()) };
        Ok(Self {
            before: JobGuard::read(before, "before")?,
            value: read(value)?,
            after: JobGuard::read(after, "after")?,
        })
    }
    fn require_original(&self, original: &JobGuard, latest: &str) -> Result<()> {
        self.before.require_qualified()?;
        self.after.require_qualified()?;
        if self.before.owner != original.owner
            || self.after.owner != original.owner
            || self.before.observed_at.as_str() < latest
            || self.after.observed_at < self.before.observed_at
        {
            return Err(invalid_data());
        }
        Ok(())
    }
}
impl Guarded<i64> {
    fn require_stop_ack(&self, original: &JobGuard, latest: &str) -> Result<()> {
        self.require_original(original, latest)?;
        if self.value == 0 { Ok(()) } else { Err(invalid_data()) }
    }
}
#[derive(Serialize)]
struct JobRuntime {
    job_id: String,
    job_name: String,
    enabled: bool,
    native_status: i64,
    native_type: i64,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RuntimeState {
    Idle,
    Executing,
    Transitional,
}
impl JobRuntime {
    fn read(row: &Rec) -> Result<Self> {
        Ok(Self {
            job_id: job_id(row)?,
            job_name: text(row, "name")?,
            enabled: boolean(row, "enabled")?,
            native_status: integer(row, "current_execution_status")?,
            native_type: integer(row, "type")?,
        })
    }
    fn state(&self, original: &JobOwner) -> Result<RuntimeState> {
        if self.job_id != original.job_id || self.job_name != original.job_name || self.enabled || self.native_type != 1
        {
            return Err(invalid_data());
        }
        // Documented current Agent runtime, not sysjobactivity's request/scheduling timestamps.
        match self.native_status {
            4 => Ok(RuntimeState::Idle),
            1 => Ok(RuntimeState::Executing),
            2 | 3 | 5 | 7 => Ok(RuntimeState::Transitional),
            _ => Err(invalid_data()),
        }
    }
}
#[derive(Default, Serialize)]
struct CaptureJobWitness {
    disabled: Option<JobGuard>,
    runtime: Vec<Guarded<JobRuntime>>,
    stop_requested: bool,
    stop: Option<Guarded<i64>>,
    settled_idle: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum JobAction {
    Complete,
    Stop,
    Observe,
}
impl CaptureJobWitness {
    fn action(&self, state: RuntimeState) -> JobAction {
        match state {
            RuntimeState::Idle => JobAction::Complete,
            RuntimeState::Executing if !self.stop_requested => JobAction::Stop,
            _ => JobAction::Observe,
        }
    }
}

pub(super) fn limits() -> Value {
    json!({
        "maximum_control_dispatches": MAX_RUNTIME_OBSERVATIONS + 2,
        "maximum_disable_requests": 1,"maximum_stop_requests": 1,
        "maximum_runtime_procedure_calls": MAX_RUNTIME_OBSERVATIONS,
        "maximum_owner_guard_selects": MAX_RUNTIME_OBSERVATIONS * 2 + 3,
        "maximum_waits": MAX_RUNTIME_OBSERVATIONS - 1,
        "maximum_runtime_observations": MAX_RUNTIME_OBSERVATIONS,
        "runtime_observation_interval_ms": RUNTIME_OBSERVATION_INTERVAL.as_millis(),
        "runtime_settlement_deadline_ms": RUNTIME_SETTLEMENT_TIMEOUT.as_millis(),
        "disable_control_deadline_ms": CONTROL_TIMEOUT.as_millis(),
        "procedure_internal_work_qualified": false,
    })
}

fn declarations(owner: Option<&JobOwner>) -> String {
    match owner {
        Some(owner) => {
            format!("DECLARE @job uniqueidentifier=N'{}',@agent int={};\n", owner.job_id, owner.agent_session_id)
        }
        None => format!(
            "DECLARE @job uniqueidentifier,@agent int;SELECT @job=j.job_id FROM ({OWNED_CAPTURE_JOB}) j;SELECT @agent=MAX(session_id) FROM msdb.dbo.syssessions;\n"
        ),
    }
}
fn qualified(disabled: bool) -> String {
    let disabled = if disabled { " AND j.enabled=0" } else { "" };
    format!(
        "@job IS NOT NULL AND @agent>0 AND @agent=(SELECT MAX(session_id) FROM msdb.dbo.syssessions) AND EXISTS(SELECT 1 FROM sys.dm_server_services WHERE servicename LIKE N'SQL Server Agent (%' AND status=4) AND EXISTS(SELECT 1 FROM ({OWNED_CAPTURE_JOB}) j WHERE j.job_id=@job{disabled})"
    )
}
fn guard_sql(phase: &str, operation_projection: &str) -> String {
    format!(
        r#"SELECT N'{phase}' AS witness_phase,CONVERT(varchar(36),@job) AS job_id,j.name AS job_name,
 DB_ID() AS database_id,ISNULL(@agent,0) AS agent_session_id,
 CONVERT(varchar(33),SYSUTCDATETIME(),126)+N'Z' AS observed_at,
 CASE WHEN EXISTS(SELECT 1 FROM ({OWNED_CAPTURE_JOB}) own WHERE own.job_id=@job) THEN 1 ELSE 0 END AS exact_owned_job,
 CASE WHEN @agent=(SELECT MAX(session_id) FROM msdb.dbo.syssessions) THEN 1 ELSE 0 END AS same_agent_session,
 CASE WHEN EXISTS(SELECT 1 FROM sys.dm_server_services WHERE servicename LIKE N'SQL Server Agent (%' AND status=4) THEN 1 ELSE 0 END AS agent_running,
 CASE WHEN j.enabled=0 THEN 1 ELSE 0 END AS job_disabled,
 CASE WHEN a.job_id IS NOT NULL THEN 1 ELSE 0 END AS activity_row_present,
 CONVERT(varchar(33),a.run_requested_date,126) AS run_requested_without_offset,
 CONVERT(varchar(33),a.queued_date,126) AS queued_without_offset,
 CONVERT(varchar(33),a.start_execution_date,126) AS execution_started_without_offset,
 CONVERT(varchar(33),a.stop_execution_date,126) AS execution_stopped_without_offset
 {operation_projection}
 FROM (VALUES(1)) witness(n) LEFT JOIN msdb.dbo.sysjobs j ON j.job_id=@job
 LEFT JOIN msdb.dbo.sysjobactivity a ON a.job_id=@job AND a.session_id=@agent;"#
    )
}
fn disable_sql() -> String {
    let observed = guard_sql("disabled", ",@return AS disable_return_code");
    format!(
        "/* culprit fixture owned capture-job disable only */ SET NOCOUNT ON;{}DECLARE @return int=NULL;IF {} EXEC @return=msdb.dbo.sp_update_job @job_id=@job,@enabled=0;{observed}",
        declarations(None),
        qualified(false)
    )
}
fn guarded_sql(owner: &JobOwner, operation: &str) -> String {
    let before = guard_sql("before", "");
    let after = guard_sql("after", "");
    format!(
        "/* culprit fixture exact original Agent runtime only */ SET NOCOUNT ON;{}{before}IF {} BEGIN {operation} END;{after}",
        declarations(Some(owner)),
        qualified(true)
    )
}
fn runtime_sql(owner: &JobOwner) -> String {
    // Client-read rows avoid nested INSERT EXEC inside the documented public procedure.
    // https://learn.microsoft.com/sql/relational-databases/system-stored-procedures/sp-help-job-transact-sql
    guarded_sql(owner, "EXEC msdb.dbo.sp_help_job @job_id=@job,@job_aspect=N'JOB';")
}
fn stop_sql(owner: &JobOwner) -> String {
    // No TRY/CATCH/number allowlist: an emitted native error always fails and fences the original owner.
    guarded_sql(
        owner,
        "DECLARE @return int;EXEC @return=msdb.dbo.sp_stop_job @job_id=@job;SELECT @return AS stop_return_code;",
    )
}

#[cfg(test)]
pub(super) fn phase_future_sizes() -> [(&'static str, usize); 4] {
    fn bytes<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    [
        ("job runtime settlement", bytes(|phase: &'static mut CaptureJobPhase<'static>| phase.settle())),
        ("job disable", bytes(|phase: &'static mut CaptureJobPhase<'static>| phase.disable())),
        (
            "job runtime observation",
            bytes(|(phase, guard): (&'static mut CaptureJobPhase<'static>, &'static JobGuard)| {
                phase.observe(guard, &guard.observed_at, CONTROL_TIMEOUT)
            }),
        ),
        (
            "job stop acknowledgement",
            bytes(|(phase, guard): (&'static mut CaptureJobPhase<'static>, &'static JobGuard)| {
                phase.stop(guard, &guard.observed_at, CONTROL_TIMEOUT)
            }),
        ),
    ]
}

#[cfg(test)]
mod tests;
