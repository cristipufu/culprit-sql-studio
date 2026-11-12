use super::*;
use ScalarType::*;

const RESOURCES: &[Field] = &[
    Field::required("SampleAt", "/record/data/at", Timestamp),
    Field::required("ReplicaId", "/record/data/replica_id", String),
    // Scope and capacity basis remain independent. Context cannot establish the subject's own pressure.
    Field::required("Metrics", "/_projection/resource_metrics", JsonContainer),
];
const WAITS: &[Field] = &[
    Field::required("NativeSource", "/record/data/source", String),
    Field::required("WaitType", "/record/data/wait_type", String),
    Field::required("MeasurementScope", "/record/data/scope", String),
    Field::required("ResetKind", "/record/data/reset/kind", String),
    Field::optional("ResetValue", "/record/data/reset/value", String),
    Field::required("WaitingTasks", "/record/data/waiting_tasks_count", ExactCounter),
    Field::required("WaitTimeMs", "/record/data/wait_time_ms", ExactCounter),
    Field::required("SignalWaitTimeMs", "/record/data/signal_wait_time_ms", ExactCounter),
    Field::required("MaxWaitTimeMs", "/record/data/max_wait_time_ms", ExactCounter),
];
const SETTINGS: &[Field] = &[
    Field::required("NativeSource", "/record/data/source", String),
    Field::required("SettingNamespace", "/record/data/namespace", String),
    Field::required("SettingName", "/record/data/name", String),
    Field::required("MeasurementScope", "/record/data/scope", String),
    Field::required("SettingNature", "/record/data/nature", String),
    Field::optional("ConfiguredValue", "/record/data/value", String),
    Field::optional("ActiveValue", "/record/data/value_in_use", String),
    Field::optional("SecondaryValue", "/record/data/value_for_secondary", String),
    Field::optional("NativeUnit", "/record/data/unit", String),
    Field::optional("NativeContext", "/record/data/native_context", String),
    Field::optional("NativeValueSource", "/record/data/native_source", String),
    Field::optional("PendingRestart", "/record/data/pending_restart", Bool),
    Field::optional("NativeIsDefault", "/record/data/native_is_default", Bool),
    Field::optional("DefaultValue", "/record/data/default_value", String),
    Field::required("DefaultSource", "/record/data/default_source", String),
];
pub fn resources(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritResourceSamplesV1", common.iter().chain(RESOURCES).copied())
}
pub fn wait_counters(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritWaitCountersV1", common.iter().chain(WAITS).copied())
}
pub fn settings(common: &[Field]) -> Result<LogicalSchema> {
    LogicalSchema::new("CulpritSettingsV1", common.iter().chain(SETTINGS).copied())
}
