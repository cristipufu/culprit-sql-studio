//! Source identity is separate from payload availability and redacted representation.
use super::{CollectionDescription, digest, valid_digest};
use crate::{Engine, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[cfg(test)]
#[path = "definitions/variant_tests.rs"]
mod variant_tests;

pub const REDACTION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeQueryIdentity {
    pub query_id: String,
    pub qualifiers: std::collections::BTreeMap<String, String>,
}
impl NativeQueryIdentity {
    pub fn native(&self) -> super::NativeIdentity {
        super::NativeIdentity { id: self.query_id.clone(), qualifiers: self.qualifiers.clone() }
    }
    pub fn definition_id(&self, source: &CollectionDescription) -> Result<String> {
        source.scoped_id("query-definition-v2", self)
    }
    pub fn is_valid(&self) -> bool {
        super::valid_key(&self.query_id) && super::native_identity::native_properties_valid(&self.qualifiers)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativePlanIdentity {
    pub plan_id: String,
    pub query: NativeQueryIdentity,
    pub qualifiers: std::collections::BTreeMap<String, String>,
}
impl NativePlanIdentity {
    pub fn native(&self) -> super::NativeIdentity {
        super::NativeIdentity { id: self.plan_id.clone(), qualifiers: self.qualifiers.clone() }
    }
    pub fn definition_id(&self, source: &CollectionDescription) -> Result<String> {
        source.scoped_id("plan-definition-v2", self)
    }
    pub fn is_valid(&self) -> bool {
        super::valid_key(&self.plan_id)
            && super::native_identity::native_properties_valid(&self.qualifiers)
            && self.query.is_valid()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PayloadUnavailable {
    Missing,
    Restricted,
    Encrypted,
    Oversized,
    RedactionFailed,
    NotCollected,
    SourceUnavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case")]
#[ts(export)]
pub enum DefinitionPayload {
    Available { text: String, sha256: String, redaction_version: u32 },
    Unavailable { reason: PayloadUnavailable },
}

impl DefinitionPayload {
    pub fn partition_bound(raw: &str, engine: Engine) -> Self {
        Self::available(crate::redact::redact_partition_bound(raw, engine))
    }
    pub fn sql(raw: &str, engine: Engine) -> Self {
        Self::available(crate::redact::redact_sql(raw, engine))
    }
    pub fn plan(raw: &str, engine: Engine) -> Self {
        let text = crate::redact::redact_native(raw, engine);
        // A failed XML redaction returns an inert comment. It is not an available plan.
        if (engine == Engine::Mssql || raw.trim_start_matches('\u{feff}').trim_start().starts_with('<'))
            && !crate::xml::read(&text, |doc| {
                doc.is_ok_and(|doc| doc.root_element().tag_name().name() == "ShowPlanXML")
            })
        {
            return Self::Unavailable { reason: PayloadUnavailable::RedactionFailed };
        }
        Self::available(text)
    }
    fn available(text: String) -> Self {
        Self::Available { sha256: digest(text.as_bytes()), text, redaction_version: REDACTION_VERSION }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Available { text, sha256, redaction_version } => {
                *redaction_version == REDACTION_VERSION && valid_digest(sha256) && *sha256 == digest(text.as_bytes())
            }
            Self::Unavailable { .. } => true,
        }
    }
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Native provenance is a fact, independent of redacted text and analytical workload selection.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StatementOrigin {
    Application,
    CulpritMonitoring,
    Culprit,
    EngineInternal,
    Unknown,
}
impl StatementOrigin {
    pub fn is_culprit(self) -> bool {
        matches!(self, Self::CulpritMonitoring | Self::Culprit)
    }
    /// Only SQL comments establish a marker. A literal containing the same words is application data.
    pub fn from_sql(text: Option<&str>, engine: Engine) -> (Self, Option<String>) {
        let Some(text) = text else { return (Self::Unknown, None) };
        let Ok(Some(comments)) = crate::sql::block_comments(text, engine) else { return (Self::Unknown, None) };
        let origin = if comments.contains(&"/* culprit:monitor */") {
            Self::CulpritMonitoring
        } else if comments.contains(&"/* culprit */") {
            Self::Culprit
        } else {
            Self::Application
        };
        let template = (origin == Self::CulpritMonitoring)
            .then(|| {
                comments.iter().find_map(|comment| {
                    comment
                        .strip_prefix("/* culprit:template:")
                        .and_then(|value| value.strip_suffix(" */"))
                        .filter(|value| valid_digest(value))
                        .map(str::to_owned)
                })
            })
            .flatten();
        (origin, template)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct QueryDefinition {
    pub id: String,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub identity: NativeQueryIdentity,
    /// Native child/dispatcher relationship observed with this exact query definition. No parent runtime is implied.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub variant_family: Option<crate::QueryVariantFamily>,
    /// Native context settings are opaque source facts, independent of localized labels.
    pub context: std::collections::BTreeMap<String, String>,
    pub context_available: bool,
    pub source_text_bytes: Option<String>,
    pub sql: DefinitionPayload,
}

impl QueryDefinition {
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id)
            && self.identity.is_valid()
            && self.variant_family.as_ref().is_none_or(|family| family.is_valid() && family.variant == self.identity)
            && self.monitoring_template_id.as_deref().is_none_or(valid_digest)
            && (self.monitoring_template_id.is_none() || self.origin == StatementOrigin::CulpritMonitoring)
            && self.sql.is_valid()
            && self.context.len() <= 32
            && self.context.iter().all(|(name, value)| name.len() <= 64 && value.len() <= 128)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanDefinition {
    pub id: String,
    pub identity: NativePlanIdentity,
    /// Absent when this native cache does not share a definition namespace with statement statistics.
    pub query_definition_id: Option<String>,
    pub engine_version: Option<String>,
    pub native_properties: std::collections::BTreeMap<String, String>,
    pub source_text_bytes: Option<String>,
    pub plan: DefinitionPayload,
}

impl PlanDefinition {
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id)
            && self.query_definition_id.as_deref().is_none_or(valid_digest)
            && self.identity.is_valid()
            && self.plan.is_valid()
            && self.engine_version.as_ref().is_none_or(|version| !version.is_empty() && version.len() <= 128)
            && super::native_identity::native_properties_valid(&self.native_properties)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct ReferenceCoverage {
    pub queries: DefinitionCoverage,
    pub plans: DefinitionCoverage,
    pub unavailable_payloads: u32,
    pub complete: bool,
    pub activity: Option<super::ActivityReferenceCoverage>,
}

/// Counts definition references; an unresolved query with no native identity is counted per native measurement.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct DefinitionCoverage {
    pub required: u32,
    pub resolved: u32,
    pub unresolved: u32,
}
impl DefinitionCoverage {
    pub fn complete(&self) -> bool {
        self.required == self.resolved && self.unresolved == 0
    }
}
