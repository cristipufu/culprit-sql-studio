//! Statement-cache identity and native provenance are shared by runtime and memory-grant observations.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CachedStatementIdentity {
    pub plan_handle: String,
    pub sql_handle: String,
    /// Native UTF-16 byte offsets, not Rust/JavaScript character offsets.
    pub statement_start_bytes: u32,
    pub statement_end_bytes: i32,
    pub created_without_offset: String,
    pub plan_generation: String,
}
impl CachedStatementIdentity {
    pub fn key(&self) -> crate::Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut canonical = self.clone();
        canonical.plan_handle.make_ascii_lowercase();
        canonical.sql_handle.make_ascii_lowercase();
        canonical.created_without_offset =
            chrono::NaiveDateTime::parse_from_str(&self.created_without_offset, "%Y-%m-%dT%H:%M:%S%.f")
                .map_err(|_| CollectionStop::InvalidPage.error())?
                .format("%Y-%m-%dT%H:%M:%S%.f")
                .to_string();
        canonical.plan_generation =
            self.plan_generation.parse::<u64>().map_err(|_| CollectionStop::InvalidPage.error())?.to_string();
        serde_json::to_vec(&canonical).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    /// Zero native handles do not qualify the DMV's placeholder counters as measured work.
    pub fn has_plan_handle(&self) -> bool {
        self.plan_handle.strip_prefix("0x").is_some_and(|hex| hex.bytes().any(|byte| byte != b'0'))
    }
    pub(super) fn is_valid(&self) -> bool {
        let handle = |value: &str| {
            value.strip_prefix("0x").is_some_and(|hex| {
                !hex.is_empty()
                    && hex.len() <= 128
                    && hex.len().is_multiple_of(2)
                    && hex.bytes().all(|v| v.is_ascii_hexdigit())
            })
        };
        handle(&self.plan_handle)
            && handle(&self.sql_handle)
            && valid_counter(&self.plan_generation)
            && self.created_without_offset.len() <= 64
            && chrono::NaiveDateTime::parse_from_str(&self.created_without_offset, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
            && self.statement_start_bytes.is_multiple_of(2)
            && (self.statement_end_bytes == -1
                || self.statement_end_bytes >= 0
                    && (self.statement_end_bytes as u32).is_multiple_of(2)
                    && self.statement_end_bytes as u32 >= self.statement_start_bytes)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CachedStatementContext {
    pub identity: CachedStatementIdentity,
    /// Native compilation database, not the batch text's nullable database or a session default.
    pub database_id: u32,
    pub query_hash: Option<String>,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub source_text_bytes: Option<String>,
    pub source_text_complete: bool,
}
impl CachedStatementContext {
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && self.database_id > 0
            && self.query_hash.as_deref().is_none_or(native_query_hash)
            && self.monitoring_template_id.as_deref().is_none_or(valid_digest)
            && (self.monitoring_template_id.is_none() || self.origin == StatementOrigin::CulpritMonitoring)
            && (!self.source_text_complete || self.source_text_bytes.is_some())
            && self.source_text_bytes.as_deref().is_none_or(valid_counter)
    }
}
pub(super) fn native_query_hash(value: &str) -> bool {
    value.strip_prefix("0x").is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CachedRuntimeMetric {
    WorkerMicroseconds,
    ElapsedMicroseconds,
    ClrMicroseconds,
    LogicalReads,
    PhysicalReads,
    LogicalWrites,
    Rows,
}
impl CachedRuntimeMetric {
    pub const ALL: [Self; 7] = [
        Self::WorkerMicroseconds,
        Self::ElapsedMicroseconds,
        Self::ClrMicroseconds,
        Self::LogicalReads,
        Self::PhysicalReads,
        Self::LogicalWrites,
        Self::Rows,
    ];
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CachedCounterDistribution {
    pub total: String,
    pub last: String,
    pub minimum: String,
    pub maximum: String,
}
impl CachedCounterDistribution {
    fn is_valid(&self) -> bool {
        [&self.total, &self.last, &self.minimum, &self.maximum].into_iter().all(|value| valid_counter(value))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CachedStatementRuntime {
    pub context: CachedStatementContext,
    pub query_plan_hash: Option<String>,
    /// Unknown for zero native plan handles, whose counters may be unsupported or disabled.
    pub executions: Option<String>,
    /// Microsecond timings have the native DMV's millisecond accuracy. No variance is inferred.
    pub counters: std::collections::BTreeMap<CachedRuntimeMetric, CachedCounterDistribution>,
    pub last_execution_without_offset: Option<String>,
    pub reset: crate::ResetMarker,
}
impl CachedStatementRuntime {
    pub fn measured(&self) -> bool {
        self.context.identity.has_plan_handle()
    }
    pub fn is_valid(&self) -> bool {
        self.context.is_valid()
            && self.query_plan_hash.as_deref().is_none_or(native_query_hash)
            && self.executions.as_deref().is_none_or(valid_counter)
            && self.executions.is_some() == self.measured()
            && if self.measured() {
                self.counters.len() == CachedRuntimeMetric::ALL.len()
                    && CachedRuntimeMetric::ALL
                        .iter()
                        .all(|metric| self.counters.get(metric).is_some_and(CachedCounterDistribution::is_valid))
            } else {
                self.counters.is_empty()
            }
            && self.last_execution_without_offset.as_deref().is_none_or(super::catalog::local_time)
            && valid_reset(&self.reset)
    }
}
