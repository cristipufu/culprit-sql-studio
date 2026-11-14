//! Native session/request observations keep blocker relationships in the same authenticated row.
use super::*;
use ScalarType::*;

const OBSERVATION: &[Field] = &[
    // Retain the complete typed metadata representation for exact authenticated reconstruction. The scalar
    // columns below remain directly queryable; payload text still follows the shared recovery inventory.
    Field::required("ActivityFacts", "/_projection/activity", JsonContainer),
    Field::required("BlockingStatus", "/_projection/activity/blocking_status", String),
    Field::optional("WaitingOnLock", "/_projection/activity/waiting_on_lock", Bool),
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
    Field::optional("OpenTransactions", "/_projection/activity/open_transactions", Long),
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
    Field::optional("WaitResources", "/_projection/activity/wait/identified_resources", JsonContainer),
    Field::optional("WaitOwners", "/_projection/activity/wait/owners", JsonContainer),
    Field::optional("QueryHash", "/_projection/activity/query_hash", String),
    Field::optional("StatementIdentity", "/_projection/activity/statement_identity", JsonContainer),
    Field::optional("ClientApplication", "/_projection/activity/client/application", String),
    Field::optional("ClientUser", "/_projection/activity/client/user", String),
    Field::optional("ClientHost", "/_projection/activity/client/host", String),
];
pub fn observations(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritActivityV1", common.iter().chain(OBSERVATION).copied())
}
