use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum TimescaleJobTimestamp {
    Finite { at: String },
    NegativeInfinity,
    PositiveInfinity,
}
impl TimescaleJobTimestamp {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Finite { at } => canonical_time(at).is_ok(),
            _ => true,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobProcedure {
    pub schema: String,
    pub name: String,
}
impl TimescaleJobProcedure {
    fn is_valid(&self) -> bool {
        label(&self.schema, MAX_TIMESCALE_JOB_LABEL_BYTES) && label(&self.name, MAX_TIMESCALE_JOB_LABEL_BYTES)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum TimescaleReportedJobState {
    Running,
    Scheduled,
    Paused,
    Unknown { native: String },
}
impl TimescaleReportedJobState {
    pub fn from_native(value: String) -> Self {
        match value.as_str() {
            "Running" => Self::Running,
            "Scheduled" => Self::Scheduled,
            "Paused" => Self::Paused,
            _ => Self::Unknown { native: value },
        }
    }
    fn is_valid(&self) -> bool {
        match self {
            Self::Unknown { native } => label(native, 128),
            _ => true,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum TimescaleJobLastRun {
    Success,
    Failed,
    Unknown { native: String },
}
impl TimescaleJobLastRun {
    pub fn from_native(value: String) -> Self {
        match value.as_str() {
            "Success" => Self::Success,
            "Failed" => Self::Failed,
            _ => Self::Unknown { native: value },
        }
    }
    fn is_valid(&self) -> bool {
        match self {
            Self::Unknown { native } => label(native, 128),
            _ => true,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobStatistics {
    pub reported_job_state: Option<TimescaleReportedJobState>,
    pub last_run_status: Option<TimescaleJobLastRun>,
    pub last_run_started_at: Option<TimescaleJobTimestamp>,
    pub last_successful_finish: Option<TimescaleJobTimestamp>,
    pub last_run_duration: Option<String>,
    pub next_start: Option<TimescaleJobTimestamp>,
    pub total_runs: Option<String>,
    pub total_successes: Option<String>,
    pub total_failures: Option<String>,
}
impl TimescaleJobStatistics {
    fn is_valid(&self) -> bool {
        self.reported_job_state.as_ref().is_none_or(TimescaleReportedJobState::is_valid)
            && self.last_run_status.as_ref().is_none_or(TimescaleJobLastRun::is_valid)
            && [&self.last_run_started_at, &self.last_successful_finish, &self.next_start]
                .into_iter()
                .flatten()
                .all(TimescaleJobTimestamp::is_valid)
            && self.last_run_duration.as_deref().is_none_or(|value| label(value, 128))
            && [&self.total_runs, &self.total_successes, &self.total_failures]
                .into_iter()
                .flatten()
                .all(|value| counter(value))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobState {
    /// Original bounded read position. Repeated native job IDs are retained without picking a representative.
    pub position: u32,
    pub observed_at: String,
    pub job_id: String,
    pub procedure: TimescaleJobProcedure,
    pub owner: String,
    pub scheduled: bool,
    pub schedule_interval: String,
    pub max_runtime: String,
    pub max_retries: String,
    pub retry_period: String,
    pub fixed_schedule: Option<bool>,
    /// Current optional configuration-validation function label, not an executed procedure or historical binding.
    #[serde(deserialize_with = "Option::deserialize")]
    pub configuration_check: Option<TimescaleJobProcedure>,
    pub initial_start: Option<TimescaleJobTimestamp>,
    pub next_start: Option<TimescaleJobTimestamp>,
    /// Native associated-object label; not an object incarnation or a historical execution binding.
    pub hypertable: Option<TimescaleJobProcedure>,
    pub statistics: Option<TimescaleJobStatistics>,
}
impl TimescaleJobState {
    pub fn is_valid(&self) -> bool {
        self.position > 0
            && self.position as usize <= MAX_TIMESCALE_JOB_OBSERVATIONS
            && canonical_time(&self.observed_at).is_ok()
            && oid(&self.job_id)
            && self.procedure.is_valid()
            && label(&self.owner, MAX_TIMESCALE_JOB_LABEL_BYTES)
            && [&self.schedule_interval, &self.max_runtime, &self.retry_period]
                .into_iter()
                .all(|value| label(value, 128))
            && self.max_retries.parse::<i32>().is_ok_and(|value| value.to_string() == self.max_retries)
            && [&self.initial_start, &self.next_start].into_iter().flatten().all(TimescaleJobTimestamp::is_valid)
            && self.configuration_check.as_ref().is_none_or(TimescaleJobProcedure::is_valid)
            && self.hypertable.as_ref().is_none_or(TimescaleJobProcedure::is_valid)
            && self.statistics.as_ref().is_none_or(TimescaleJobStatistics::is_valid)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&("timescale-job-state-v1", self.position, &self.job_id, &self.procedure))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TimescaleJobProcedureOrigin {
    Stored,
    StoredOrCurrent,
    Unknown,
}

impl TimescaleJobProcedureOrigin {
    /// Only a verified native informational-view contract qualifies label provenance, never definition identity.
    pub fn for_view(version: &str, view: TimescaleJobViewKind) -> Self {
        match (version, view) {
            ("2.31.0", TimescaleJobViewKind::History) => Self::StoredOrCurrent,
            ("2.31.0", TimescaleJobViewKind::Errors) => Self::Stored,
            _ => Self::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleReportedJobProcedure {
    pub label: TimescaleJobProcedure,
    pub origin: TimescaleJobProcedureOrigin,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobRun {
    pub position: u32,
    pub observed_at: String,
    /// History exposes an exact native run ID; job_errors does not. Neither grants cross-capture incarnation.
    pub native_run_id: Option<String>,
    pub job_id: String,
    /// Informational views may coalesce stored labels with current definitions; this is never a binding.
    pub procedure: Option<TimescaleReportedJobProcedure>,
    pub process_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// NULL is unfinished or otherwise unverified; membership in job_errors never changes it to false.
    pub succeeded: Option<bool>,
    pub sql_state: Option<String>,
}
impl TimescaleJobRun {
    pub fn is_valid(&self) -> bool {
        self.position > 0
            && self.position as usize <= MAX_TIMESCALE_JOB_OBSERVATIONS
            && canonical_time(&self.observed_at).is_ok()
            && oid(&self.job_id)
            && self.native_run_id.as_deref().is_none_or(counter)
            && self.procedure.as_ref().is_none_or(|procedure| procedure.label.is_valid())
            && self.process_id.as_deref().is_none_or(oid)
            && canonical_time(&self.started_at).is_ok()
            && self.finished_at.as_ref().is_none_or(|until| ObservationBounds::new(&self.started_at, until).is_some())
            && self.sql_state.as_ref().is_none_or(|code| {
                code.len() == 5 && code.bytes().all(|value| value.is_ascii_digit() || value.is_ascii_uppercase())
            })
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(
            "timescale-job-run-v1",
            self.position,
            &self.native_run_id,
            &self.job_id,
            &self.started_at,
        ))
        .map(|bytes| digest(&bytes))
        .map_err(|_| invalid())
    }
    pub fn failure_kind(&self) -> Option<PostgresFailureKind> {
        (self.succeeded == Some(false) && self.finished_at.is_some())
            .then(|| self.sql_state.as_deref().map(PostgresFailureKind::from_sql_state))
            .flatten()
    }
}
