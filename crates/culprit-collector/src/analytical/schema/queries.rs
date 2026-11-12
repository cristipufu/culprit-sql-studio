//! Query measurements expose native identity, optional definitions and explicitly unit-bearing metrics.
use super::*;
use ScalarType::*;

const KEY: &[Field] = &[
    Field::required("NativeRowId", "/record/data/key/native/id", String),
    Field::required("NativeDimensions", "/record/data/key/native/qualifiers", JsonContainer),
    Field::required("NativeReferences", "/record/data/key/references", JsonContainer),
    Field::optional("QueryDefinitionId", "/record/data/key/references/query/definition_id", String),
    Field::optional("PlanDefinitionId", "/record/data/key/references/plan/definition_id", String),
];
const RUNTIME: &[Field] = &[
    Field::required("Executions", "/record/data/executions", ExactCounter),
    Field::required("Metrics", "/record/data/metrics", JsonContainer),
    Field::optional("FirstExecutionAt", "/record/data/first_execution_at", Timestamp),
    Field::optional("LastExecutionAt", "/record/data/last_execution_at", Timestamp),
];
const WAITS: &[Field] = &[
    Field::required("WaitCategory", "/record/data/category", String),
    Field::required("TimeUnit", "/record/data/unit/kind", String),
    Field::required("TotalWait", "/record/data/total", JsonContainer),
    Field::optional("AverageWait", "/record/data/average", Real),
    Field::optional("MinimumWait", "/record/data/minimum", JsonContainer),
    Field::optional("MaximumWait", "/record/data/maximum", JsonContainer),
];
pub fn runtime(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritQueryRuntimeV1", common.iter().chain(KEY).chain(RUNTIME).copied())
}
pub fn waits(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritQueryWaitsV1", common.iter().chain(KEY).chain(WAITS).copied())
}
