use super::*;
use ScalarType::*;

const KEY: &[Field] = &[
    Field::required("NativeRowId", "/record/data/key/row_id", String),
    Field::required("NativePlanId", "/record/data/key/plan_id", String),
    Field::required("ReplicaGroupId", "/record/data/key/replica_group_id", String),
    Field::required("ExecutionOutcome", "/record/data/key/execution_type", Long),
    Field::optional("PlanDefinitionId", "/record/data/key/plan_definition_id", String),
];
const RUNTIME: &[Field] = &[
    Field::required("Executions", "/record/data/executions", ExactCounter),
    Field::required("AvgDurationUs", "/record/data/avg_duration_us", Real),
    Field::required("StdevDurationUs", "/record/data/stdev_duration_us", Real),
    Field::required("MinDurationUs", "/record/data/min_duration_us", Real),
    Field::required("MaxDurationUs", "/record/data/max_duration_us", Real),
    Field::required("AvgCpuUs", "/record/data/avg_cpu_us", Real),
    Field::required("AvgLogicalReads8KbPages", "/record/data/avg_logical_reads_8kb_pages", Real),
    Field::required("AvgPhysicalReads8KbPages", "/record/data/avg_physical_reads_8kb_pages", Real),
    Field::optional("AvgLogicalWrites8KbPages", "/record/data/avg_logical_writes_8kb_pages", Real),
    Field::optional("AvgRows", "/record/data/avg_rows", Real),
    Field::optional("AvgDop", "/record/data/avg_dop", Real),
    Field::optional("AvgMaxUsedMemory8KbPages", "/record/data/avg_max_used_memory_8kb_pages", Real),
    Field::optional("AvgClrUs", "/record/data/avg_clr_us", Real),
    Field::optional("AvgLogBytes", "/record/data/avg_log_bytes", Real),
    Field::optional("AvgTempdb8KbPages", "/record/data/avg_tempdb_8kb_pages", Real),
    Field::required("FirstExecutionAt", "/record/data/first_execution_at", Timestamp),
    Field::required("LastExecutionAt", "/record/data/last_execution_at", Timestamp),
];
const WAITS: &[Field] = &[
    Field::required("WaitCategory", "/record/data/category", Long),
    Field::required("TotalWaitMs", "/record/data/total_wait_ms", ExactCounter),
    Field::required("AvgWaitMs", "/record/data/avg_wait_ms", Real),
    Field::required("MinWaitMs", "/record/data/min_wait_ms", ExactCounter),
    Field::required("MaxWaitMs", "/record/data/max_wait_ms", ExactCounter),
];
pub fn runtime(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritQueryRuntimeV1", common.iter().chain(KEY).chain(RUNTIME).copied())
}
pub fn waits(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritQueryWaitsV1", common.iter().chain(KEY).chain(WAITS).copied())
}
