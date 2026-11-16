//! Positive variable-input classifications retained before native plan privacy removes database namespaces.
use super::*;
use crate::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};
use serde::Deserializer;
use serde::de::{SeqAccess, Visitor};
use std::fmt;

pub const MAX_MSSQL_RELATIONAL_INPUT_BINDINGS: usize = 64;
pub const MAX_MSSQL_RELATIONAL_INPUT_PROOF_BYTES: usize = 8 * 1024;
const SCOPE_DOMAIN: &str = "mssql-relational-input-scope-v1";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlRelationalInputBinding {
    pub node_id: u32,
    pub input: RelationalInput,
}

/// This binds a source-owned classification, not an execution, counter lifetime, input cardinality or harm claim.
/// The original row receipt authenticates these facts; the hashes prevent substitution of its native inputs.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlRelationalInputProof {
    pub source_scope: String,
    pub input_sha256: String,
    #[serde(deserialize_with = "bindings")]
    pub bindings: Vec<MssqlRelationalInputBinding>,
}

impl MssqlRelationalInputProof {
    pub fn supports_source(source: &CollectionDescription) -> bool {
        scope(source).is_ok()
    }
    pub fn for_cached(
        source: &CollectionDescription,
        definition: &MssqlCachedDefinition,
        mut bindings: Vec<MssqlRelationalInputBinding>,
    ) -> Result<Self> {
        if !definition.is_valid() {
            return Err(invalid());
        }
        bindings.sort_by_key(|binding| binding.node_id);
        let source_scope = scope(source)?;
        let value = Self { input_sha256: cached_input(&source_scope, definition, &bindings)?, source_scope, bindings };
        if !value.matches_cached(source, definition) {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn for_retained(
        source: &CollectionDescription,
        definition: &MssqlRetainedPlanDefinition,
        mut bindings: Vec<MssqlRelationalInputBinding>,
    ) -> Result<Self> {
        if !definition.is_valid() {
            return Err(invalid());
        }
        bindings.sort_by_key(|binding| binding.node_id);
        let source_scope = scope(source)?;
        let value =
            Self { input_sha256: retained_input(&source_scope, definition, &bindings)?, source_scope, bindings };
        if !value.matches_retained(source, definition) {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn is_valid(&self) -> bool {
        valid_digest(&self.source_scope)
            && valid_digest(&self.input_sha256)
            && (1..=MAX_MSSQL_RELATIONAL_INPUT_BINDINGS).contains(&self.bindings.len())
            && self.bindings.windows(2).all(|pair| pair[0].node_id < pair[1].node_id)
            && self.bindings.iter().all(|binding| {
                binding.input.is_valid()
                    && matches!(
                        (binding.input.kind, binding.input.provenance),
                        (RelationalInputKind::TableValuedParameter, RelationalInputProvenance::NativeParameterMetadata)
                            | (RelationalInputKind::LocalTableVariable, RelationalInputProvenance::NativeDeclaration)
                    )
            })
            && crate::serialization::json_size(self, MAX_MSSQL_RELATIONAL_INPUT_PROOF_BYTES).is_ok()
    }

    pub fn matches_cached(&self, source: &CollectionDescription, definition: &MssqlCachedDefinition) -> bool {
        source_dataset(source) == Some(SnapshotDataset::CachedDefinitions)
            && scope(source).is_ok_and(|scope| scope == self.source_scope)
            && self.matches_cached_contents(definition)
    }

    pub fn matches_retained(&self, source: &CollectionDescription, definition: &MssqlRetainedPlanDefinition) -> bool {
        source_dataset(source) == Some(SnapshotDataset::RetainedPlans)
            && scope(source).is_ok_and(|scope| scope == self.source_scope)
            && self.matches_retained_contents(definition)
    }

    pub(super) fn matches_cached_contents(&self, definition: &MssqlCachedDefinition) -> bool {
        self.inputs_available(&definition.statement, &definition.estimated_plan, definition.module_parameters.as_ref())
            && cached_input(&self.source_scope, definition, &self.bindings)
                .is_ok_and(|expected| expected == self.input_sha256)
    }

    pub(super) fn matches_retained_contents(&self, definition: &MssqlRetainedPlanDefinition) -> bool {
        self.inputs_available(&definition.statement, &definition.batch_plan, definition.module_parameters.as_ref())
            && retained_input(&self.source_scope, definition, &self.bindings)
                .is_ok_and(|expected| expected == self.input_sha256)
    }

    fn inputs_available(
        &self,
        statement: &MssqlCachedPayload,
        plan: &MssqlCachedPayload,
        parameters: Option<&MssqlModuleParameters>,
    ) -> bool {
        self.is_valid()
            && available_native(plan)
            && (self.bindings.iter().all(|binding| binding.input.kind != RelationalInputKind::TableValuedParameter)
                || available_native(statement)
                    && parameters.is_some_and(|parameters| !parameters.table_parameters.is_empty()))
    }
}

impl ObjectDefinitionFact {
    pub fn relational_input_proof_matches(&self, source: &CollectionDescription) -> bool {
        match self {
            Self::MssqlCachedDefinition(value) => {
                value.relational_input_proof.as_ref().is_none_or(|proof| proof.matches_cached(source, value))
            }
            Self::MssqlRetainedPlan(value) => {
                value.relational_input_proof.as_ref().is_none_or(|proof| proof.matches_retained(source, value))
            }
            _ => true,
        }
    }
}

fn scope(source: &CollectionDescription) -> Result<String> {
    if source.target.engine() != crate::Engine::Mssql
        || source.target.database().is_none()
        || !source.is_partition_qualified()
        || !matches!(source.partition, SourcePartition::InstanceLocal { .. })
    {
        return Err(invalid());
    }
    source.scoped_id(SCOPE_DOMAIN, &())
}
fn source_dataset(source: &CollectionDescription) -> Option<SnapshotDataset> {
    match &source.observation {
        SourceObservation::Snapshot(snapshot) => Some(snapshot.dataset),
        SourceObservation::PagedSnapshot(snapshot) => Some(snapshot.dataset),
        _ => None,
    }
}

#[derive(Serialize)]
struct PayloadBinding<'a> {
    source_utf16_bytes: Option<&'a str>,
    native_sha256: Option<&'a str>,
    representation: Representation<'a>,
}
#[derive(Serialize)]
enum Representation<'a> {
    Available { sha256: &'a str, redaction_version: u32 },
    Unavailable(PayloadUnavailable),
}
impl<'a> From<&'a MssqlCachedPayload> for PayloadBinding<'a> {
    fn from(value: &'a MssqlCachedPayload) -> Self {
        Self {
            source_utf16_bytes: value.source_utf16_bytes.as_deref(),
            native_sha256: value.native_sha256.as_deref(),
            representation: match &value.payload {
                DefinitionPayload::Available { sha256, redaction_version, .. } => {
                    Representation::Available { sha256, redaction_version: *redaction_version }
                }
                DefinitionPayload::Unavailable { reason } => Representation::Unavailable(*reason),
            },
        }
    }
}

fn cached_input(
    scope: &str,
    value: &MssqlCachedDefinition,
    bindings: &[MssqlRelationalInputBinding],
) -> Result<String> {
    encoded_digest(&(
        "mssql-cached-relational-input-v1",
        scope,
        &value.context,
        &value.query_plan_hash,
        &value.module_parameters,
        PayloadBinding::from(&value.statement),
        PayloadBinding::from(&value.estimated_plan),
        bindings,
    ))
}
fn retained_input(
    scope: &str,
    value: &MssqlRetainedPlanDefinition,
    bindings: &[MssqlRelationalInputBinding],
) -> Result<String> {
    encoded_digest(&(
        "mssql-retained-relational-input-v1",
        scope,
        &value.context,
        &value.module_parameters,
        PayloadBinding::from(&value.statement),
        PayloadBinding::from(&value.batch_plan),
        bindings,
    ))
}
fn encoded_digest(value: &impl Serialize) -> Result<String> {
    serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}
fn available_native(value: &MssqlCachedPayload) -> bool {
    value.is_valid()
        && value.payload.is_available()
        && value.source_utf16_bytes.as_deref().is_some_and(|bytes| bytes.parse::<u64>().is_ok_and(|bytes| bytes > 0))
        && value.native_sha256.is_some()
}
fn bindings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<MssqlRelationalInputBinding>, D::Error> {
    struct Bindings;
    impl<'de> Visitor<'de> for Bindings {
        type Value = Vec<MssqlRelationalInputBinding>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded native relational input bindings")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element()? {
                if values.len() == MAX_MSSQL_RELATIONAL_INPUT_BINDINGS {
                    return Err(serde::de::Error::custom("native relational input binding limit exceeded"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Bindings)
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
