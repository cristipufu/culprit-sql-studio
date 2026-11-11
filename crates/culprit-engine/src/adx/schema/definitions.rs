use super::*;
use ScalarType::*;

const QUERY: &[Field] = &[
    Field::required("DefinitionId", "/record/data/id", String),
    Field::required("NativeQueryId", "/record/data/identity/query_id", String),
    Field::required("ContextSettingsId", "/record/data/identity/context_settings_id", String),
    Field::required("NativeObjectId", "/record/data/identity/object_id", String),
    Field::required("QueryHash", "/record/data/identity/query_hash", String),
    Field::required("InitialCompileAt", "/record/data/identity/initial_compile_at", Timestamp),
    Field::required("StatementOrigin", "/record/data/origin", String),
    Field::optional("MonitoringTemplateId", "/record/data/monitoring_template_id", String),
    Field::required("CompilationContext", "/record/data/context", JsonContainer),
    Field::required("CompilationContextAvailable", "/record/data/context_available", Bool),
    Field::optional("NativeSourceTextBytes", "/record/data/source_text_bytes", ExactCounter),
    Field::required("SqlPayloadState", "/record/data/sql/state", String),
    Field::optional("SqlPayloadSha256", "/record/data/sql/sha256", String),
    Field::optional("SqlRedactionVersion", "/record/data/sql/redaction_version", Long),
    Field::optional("SqlPayloadUnavailableReason", "/record/data/sql/reason", String),
];
const PLAN: &[Field] = &[
    Field::required("DefinitionId", "/record/data/id", String),
    Field::required("NativePlanId", "/record/data/identity/plan_id", String),
    Field::required("NativeQueryId", "/record/data/identity/query/query_id", String),
    Field::required("ContextSettingsId", "/record/data/identity/query/context_settings_id", String),
    Field::required("NativeObjectId", "/record/data/identity/query/object_id", String),
    Field::required("QueryHash", "/record/data/identity/query/query_hash", String),
    Field::required("QueryInitialCompileAt", "/record/data/identity/query/initial_compile_at", Timestamp),
    Field::required("PlanHash", "/record/data/identity/plan_hash", String),
    Field::required("InitialCompileAt", "/record/data/identity/initial_compile_at", Timestamp),
    Field::required("QueryDefinitionId", "/record/data/query_definition_id", String),
    Field::required("EngineVersion", "/record/data/engine_version", String),
    Field::required("CompatibilityLevel", "/record/data/compatibility_level", Long),
    Field::optional("NativeSourceTextBytes", "/record/data/source_text_bytes", ExactCounter),
    Field::required("PlanPayloadState", "/record/data/plan/state", String),
    Field::optional("PlanPayloadSha256", "/record/data/plan/sha256", String),
    Field::optional("PlanRedactionVersion", "/record/data/plan/redaction_version", Long),
    Field::optional("PlanPayloadUnavailableReason", "/record/data/plan/reason", String),
];
pub fn queries(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritQueryDefinitionsV1", common.iter().chain(QUERY).copied())
}
pub fn plans(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritPlanDefinitionsV1", common.iter().chain(PLAN).copied())
}
