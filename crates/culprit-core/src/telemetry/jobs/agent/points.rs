use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentJobActivity {
    /// An msdb Agent startup-session ID. It is never a SQL session ID/SPID.
    pub agent_session_id: String,
    pub requested_without_offset: Option<String>,
    pub request_source: Option<String>,
    pub queued_without_offset: Option<String>,
    pub started_without_offset: Option<String>,
    pub stopped_without_offset: Option<String>,
    pub last_executed_step_id: Option<String>,
    pub last_executed_step_without_offset: Option<String>,
    pub next_scheduled_without_offset: Option<String>,
}
impl MssqlAgentJobActivity {
    fn is_valid(&self) -> bool {
        positive_i32(&self.agent_session_id)
            && self.request_source.as_ref().is_none_or(|value| native_text(value, 512))
            && self.last_executed_step_id.as_ref().is_none_or(|value| nonnegative_i32(value))
            && [
                &self.requested_without_offset,
                &self.queued_without_offset,
                &self.started_without_offset,
                &self.stopped_without_offset,
                &self.last_executed_step_without_offset,
                &self.next_scheduled_without_offset,
            ]
            .into_iter()
            .all(|value| value.as_ref().is_none_or(|value| native_local_time(value)))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentJob {
    pub observed_at: String,
    pub job_id: String,
    pub name: String,
    pub enabled: bool,
    pub start_step_id: String,
    pub version_number: String,
    pub created_without_offset: Option<String>,
    pub modified_without_offset: Option<String>,
    pub activity: Option<MssqlAgentJobActivity>,
}
impl MssqlAgentJob {
    pub fn key(&self) -> Result<String> {
        self.is_valid().then(|| digest(self.job_id.as_bytes())).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && native_guid(&self.job_id)
            && native_label(&self.name, MAX_MSSQL_AGENT_LABEL_BYTES)
            && nonnegative_i32(&self.start_step_id)
            && nonnegative_i32(&self.version_number)
            && [&self.created_without_offset, &self.modified_without_offset]
                .into_iter()
                .all(|value| value.as_ref().is_none_or(|value| native_local_time(value)))
            && self.activity.as_ref().is_none_or(MssqlAgentJobActivity::is_valid)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentStep {
    pub observed_at: String,
    pub job_id: String,
    pub step_id: String,
    pub name: String,
    pub subsystem: String,
    /// Native stored field. Only TSQL gives it starting-database meaning; it never restricts command access.
    pub database_name: Option<String>,
}
impl MssqlAgentStep {
    pub fn key(&self) -> Result<String> {
        self.is_valid().then(|| digest(format!("{}:{}", self.job_id, self.step_id).as_bytes())).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && native_guid(&self.job_id)
            && positive_i32(&self.step_id)
            && native_label(&self.name, MAX_MSSQL_AGENT_LABEL_BYTES)
            && native_label(&self.subsystem, 160)
            && self.database_name.as_ref().is_none_or(|value| native_text(value, MAX_MSSQL_AGENT_LABEL_BYTES))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentHistory {
    pub observed_at: String,
    pub job_id: String,
    pub instance_id: String,
    /// Zero is the native job-summary row. Positive values name the recorded step, not its current definition.
    pub step_id: String,
    pub stored_step_name: Option<String>,
    pub run_status: i32,
    /// Native integer date/time/duration encodings. Zero is preserved without interpreting it as missing or success.
    pub run_date: String,
    pub run_time: String,
    pub run_duration: String,
    pub sql_message_id: String,
    pub sql_severity: String,
    pub retries_attempted: String,
    pub reported_server: Option<String>,
}
impl MssqlAgentHistory {
    pub fn key(&self) -> Result<String> {
        self.is_valid().then(|| digest(self.instance_id.as_bytes())).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && native_guid(&self.job_id)
            && positive_i32(&self.instance_id)
            && nonnegative_i32(&self.step_id)
            && self.stored_step_name.as_ref().is_none_or(|value| native_text(value, MAX_MSSQL_AGENT_LABEL_BYTES))
            && [self.run_date.as_str(), &self.run_time, &self.run_duration, &self.retries_attempted]
                .into_iter()
                .all(nonnegative_i32)
            && signed_i32(&self.sql_message_id)
            && signed_i32(&self.sql_severity)
            && self
                .run_time
                .parse::<u32>()
                .is_ok_and(|value| value / 10000 <= 23 && value / 100 % 100 <= 59 && value % 100 <= 59)
            && self.run_duration.parse::<u32>().is_ok_and(|value| value / 100 % 100 <= 59 && value % 100 <= 59)
            && self.reported_server.as_ref().is_none_or(|value| native_text(value, MAX_MSSQL_AGENT_LABEL_BYTES))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlAgentContextBound {
    Before,
    After,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentSession {
    pub session_id: String,
    pub started_without_offset: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentService {
    pub name: String,
    pub status: Option<i32>,
    pub started_with_offset: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentContext {
    pub observed_at: String,
    pub bound: MssqlAgentContextBound,
    pub session: Option<MssqlAgentSession>,
    pub services: Vec<MssqlAgentService>,
    pub services_limited: bool,
}
impl MssqlAgentContext {
    pub fn key(&self) -> Result<String> {
        self.is_valid()
            .then(|| {
                digest(match self.bound {
                    MssqlAgentContextBound::Before => b"before",
                    MssqlAgentContextBound::After => b"after",
                })
            })
            .ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        let mut names = BTreeSet::new();
        canonical_time(&self.observed_at).is_ok()
            && self.services.len() <= MAX_MSSQL_AGENT_SERVICES
            && self.services_limited == (self.services.len() == MAX_MSSQL_AGENT_SERVICES)
            && self.session.as_ref().is_none_or(|session| {
                positive_i32(&session.session_id)
                    && session.started_without_offset.as_ref().is_none_or(|value| native_local_time(value))
            })
            && self.services.iter().all(|service| {
                native_label(&service.name, MAX_MSSQL_AGENT_LABEL_BYTES)
                    && names.insert(&service.name)
                    && service
                        .started_with_offset
                        .as_ref()
                        .is_none_or(|value| value.len() <= 48 && chrono::DateTime::parse_from_rfc3339(value).is_ok())
            })
    }
}
