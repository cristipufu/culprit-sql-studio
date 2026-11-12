//! Activity and incident rows share one projection of native session/request facts.
use super::*;
use ScalarType::*;

const OBSERVATION: &[Field] = &[
    Field::required("SessionId", "/_projection/activity/key/session/id", String),
    Field::required("SessionGeneration", "/_projection/activity/key/session/generation", String),
    Field::optional("RequestId", "/_projection/activity/key/request/id", String),
    Field::optional("RequestGeneration", "/_projection/activity/key/request/generation", String),
    Field::optional("TransactionId", "/_projection/activity/key/transaction", String),
    Field::required("Idle", "/_projection/activity/idle", Bool),
    Field::required("ScopeBasis", "/_projection/activity/scope", String),
    Field::required("NativeState", "/_projection/activity/state", String),
    Field::optional("NativeDatabase", "/_projection/activity/database", String),
    Field::optional("Command", "/_projection/activity/command", String),
    Field::optional("LastRequestAgeMs", "/_projection/activity/last_request_age_ms", ExactCounter),
    Field::required("OpenTransactions", "/_projection/activity/open_transactions", Long),
    Field::optional("ElapsedMs", "/_projection/activity/elapsed_ms", ExactCounter),
    Field::optional("TransactionAgeMs", "/_projection/activity/transaction_age_ms", ExactCounter),
    Field::optional("SessionCpuMs", "/_projection/activity/session_counters/cpu_ms", ExactCounter),
    Field::optional("SessionLogicalReads", "/_projection/activity/session_counters/logical_reads", ExactCounter),
    Field::optional("SessionPhysicalReads", "/_projection/activity/session_counters/physical_reads", ExactCounter),
    Field::optional("SessionWrites", "/_projection/activity/session_counters/writes", ExactCounter),
    Field::optional("RequestCpuMs", "/_projection/activity/request_counters/cpu_ms", ExactCounter),
    Field::optional("RequestLogicalReads", "/_projection/activity/request_counters/logical_reads", ExactCounter),
    Field::optional("RequestPhysicalReads", "/_projection/activity/request_counters/physical_reads", ExactCounter),
    Field::optional("RequestWrites", "/_projection/activity/request_counters/writes", ExactCounter),
    Field::optional("WaitType", "/_projection/activity/wait/native_type", String),
    Field::optional("WaitElapsedMs", "/_projection/activity/wait/elapsed_ms", ExactCounter),
    Field::optional("WaitOwners", "/_projection/activity/wait/owners", JsonContainer),
    Field::optional("QueryHash", "/_projection/activity/query_hash", String),
    Field::optional("ClientApplication", "/_projection/activity/client/application", String),
    Field::optional("ClientUser", "/_projection/activity/client/user", String),
    Field::optional("ClientHost", "/_projection/activity/client/host", String),
];
const INCIDENT: &[Field] = &[
    Field::required("ChangeId", "/record/data/id", String),
    Field::required("ChangedAt", "/record/data/at", Timestamp),
    Field::required("Transition", "/record/data/transition", String),
    Field::required("IncidentId", "/record/data/state/id", String),
    Field::required("FirstSeenAt", "/record/data/state/first_seen_at", Timestamp),
    Field::required("LastSeenAt", "/record/data/state/last_seen_at", Timestamp),
    Field::required("LastEmittedAt", "/record/data/state/last_emitted_at", Timestamp),
    Field::required("Conditions", "/record/data/state/conditions", JsonContainer),
    Field::required("Progress", "/record/data/state/progress", String),
];
const BLOCKING: &[Field] = &[
    Field::required("WaiterSessionId", "/record/data/waiter/session/id", String),
    Field::required("WaiterSessionGeneration", "/record/data/waiter/session/generation", String),
    Field::required("WaiterRequestId", "/record/data/waiter/request/id", String),
    Field::required("WaiterRequestGeneration", "/record/data/waiter/request/generation", String),
    Field::required("OwnerKind", "/record/data/owner/kind", String),
    Field::optional("OwnerSessionId", "/record/data/owner/id", String),
    Field::optional("OwnerSessionGeneration", "/record/data/owner/generation", String),
    Field::optional("OwnerTransactionId", "/record/data/owner/native_id", String),
    Field::optional("TransactionOwner", "/record/data/owner/owner", String),
    Field::required("OwnerObserved", "/record/data/owner_observed", Bool),
];
pub fn observations(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritActivityV1", common.iter().chain(OBSERVATION).copied())
}
pub fn incidents(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritActivityIncidentsV1", common.iter().chain(INCIDENT).chain(OBSERVATION).copied())
}
pub fn blocking(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritBlockingV1", common.iter().chain(BLOCKING).copied())
}
