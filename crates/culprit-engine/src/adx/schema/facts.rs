//! Versioned provider extensions preserve native facts; large details have explicit recovery references.
use super::*;
use ScalarType::*;

const FACT: &[Field] = &[
    Field::required("NativeSource", "/record/data/source", String),
    Field::required("FactKind", "/record/data/fact/kind", String),
    Field::required("FactKey", "/_projection/fact_key", String),
    Field::required("DetailState", "/_projection/detail_state", String),
    Field::required("DetailSha256", "/_projection/detail_sha256", String),
    Field::optional("Details", "/_projection/details", JsonContainer),
];
const OBJECT: &[Field] = &[Field::required("DefinitionVersion", "/record/data/version", String)];
const DEADLOCK: &[Field] = &[
    Field::required("EventPosition", "/record/data/position", String),
    Field::required("FileId", "/record/data/file_id", String),
    Field::required("FileOffset", "/record/data/file_offset", ExactCounter),
    Field::required("GraphOrdinal", "/record/data/graph_ordinal", Long),
    Field::required("EventAt", "/record/data/at", Timestamp),
    Field::required("Pattern", "/record/data/pattern", String),
    Field::required("ProcessCount", "/_projection/process_count", Long),
    Field::required("ResourceCount", "/_projection/resource_count", Long),
    Field::required("VictimCount", "/_projection/victim_count", Long),
    Field::required("DetailState", "/_projection/detail_state", String),
    Field::required("DetailSha256", "/_projection/detail_sha256", String),
    Field::optional("Details", "/_projection/details", JsonContainer),
];
pub fn operational(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritOperationalV1", common.iter().chain(FACT).copied())
}
pub fn replication(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritReplicationV1", common.iter().chain(FACT).copied())
}
pub fn tuning(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritTuningV1", common.iter().chain(FACT).copied())
}
pub fn objects(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritObjectsV1", common.iter().chain(FACT).chain(OBJECT).copied())
}
pub fn deadlocks(common: &[Field]) -> Result<Schema> {
    Schema::new("CulpritDeadlocksV1", common.iter().chain(DEADLOCK).copied())
}
