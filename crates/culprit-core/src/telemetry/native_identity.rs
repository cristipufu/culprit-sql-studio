//! Native identifiers remain source-owned; common collection code interprets no provider-specific components.
use super::*;
use std::collections::BTreeMap;

pub const MAX_NATIVE_QUALIFIERS: usize = 32;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NativeIdentity {
    pub id: String,
    pub qualifiers: BTreeMap<String, String>,
}
impl NativeIdentity {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), qualifiers: BTreeMap::new() }
    }
    pub fn qualified(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.qualifiers.insert(name.into(), value.into());
        self
    }
    pub fn is_valid(&self) -> bool {
        valid_key(&self.id) && native_properties_valid(&self.qualifiers)
    }
}
pub(super) fn native_properties_valid(properties: &BTreeMap<String, String>) -> bool {
    properties.len() <= MAX_NATIVE_QUALIFIERS
        && properties.iter().all(|(key, value)| valid_key(key) && valid_key(value))
}

/// A known native entity can remain unresolved when its definition was not retained or cannot be read.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NativeDefinitionReference {
    pub native: NativeIdentity,
    pub definition_id: Option<String>,
}
impl NativeDefinitionReference {
    pub fn is_valid(&self) -> bool {
        self.native.is_valid() && self.definition_id.as_deref().is_none_or(valid_digest)
    }
    pub fn is_resolved(&self) -> bool {
        self.definition_id.is_some()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatementReferences {
    /// None means that even the native query identity could not be resolved.
    pub query: Option<NativeDefinitionReference>,
    /// None means the source supplies no plan for this measurement. An unresolved known plan is explicit.
    pub plan: Option<NativeDefinitionReference>,
}
impl StatementReferences {
    pub fn is_valid(&self) -> bool {
        self.query.as_ref().is_none_or(NativeDefinitionReference::is_valid)
            && self.plan.as_ref().is_none_or(NativeDefinitionReference::is_valid)
            && (self.plan.as_ref().is_none_or(|plan| !plan.is_resolved())
                || self.query.as_ref().is_some_and(NativeDefinitionReference::is_resolved))
    }
    pub fn is_resolved(&self) -> bool {
        self.query.as_ref().is_some_and(NativeDefinitionReference::is_resolved)
            && self.plan.as_ref().is_none_or(NativeDefinitionReference::is_resolved)
    }
}
