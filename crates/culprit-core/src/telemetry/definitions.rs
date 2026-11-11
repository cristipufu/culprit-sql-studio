//! Source identity is separate from payload availability and redacted representation.
use super::{CollectionDescription, digest, valid_digest};
use crate::{Engine, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const REDACTION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeQueryIdentity {
    pub query_id: String,
    pub context_settings_id: String,
    pub object_id: String,
    pub query_hash: String,
    pub initial_compile_at: String,
}

impl NativeQueryIdentity {
    pub fn definition_id(&self, source: &CollectionDescription) -> Result<String> {
        let mut identity = self.clone();
        identity.initial_compile_at = super::canonical_time(&identity.initial_compile_at)?;
        source.scoped_id("query-definition-v1", &identity)
    }
    pub fn is_valid(&self) -> bool {
        [&self.query_id, &self.context_settings_id, &self.object_id, &self.query_hash]
            .into_iter()
            .all(|value| !value.is_empty() && value.len() <= 128)
            && chrono::DateTime::parse_from_rfc3339(&self.initial_compile_at).is_ok()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativePlanIdentity {
    pub plan_id: String,
    pub query: NativeQueryIdentity,
    pub plan_hash: String,
    pub initial_compile_at: String,
}

impl NativePlanIdentity {
    pub fn definition_id(&self, source: &CollectionDescription) -> Result<String> {
        let mut identity = self.clone();
        identity.initial_compile_at = super::canonical_time(&identity.initial_compile_at)?;
        identity.query.initial_compile_at = super::canonical_time(&identity.query.initial_compile_at)?;
        source.scoped_id("plan-definition-v1", &identity)
    }
    pub fn is_valid(&self) -> bool {
        self.query.is_valid()
            && [&self.plan_id, &self.plan_hash].into_iter().all(|value| !value.is_empty() && value.len() <= 128)
            && chrono::DateTime::parse_from_rfc3339(&self.initial_compile_at).is_ok()
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
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct QueryDefinition {
    pub id: String,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub identity: NativeQueryIdentity,
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
    pub query_definition_id: String,
    pub engine_version: String,
    pub compatibility_level: u16,
    pub source_text_bytes: Option<String>,
    pub plan: DefinitionPayload,
}

impl PlanDefinition {
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id)
            && valid_digest(&self.query_definition_id)
            && self.identity.is_valid()
            && self.plan.is_valid()
            && !self.engine_version.is_empty()
            && self.engine_version.len() <= 128
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct ReferenceCoverage {
    pub required_plans: u32,
    pub resolved_plans: u32,
    pub unresolved_plans: u32,
    pub unavailable_payloads: u32,
    pub complete: bool,
    pub activity: Option<super::ActivityReferenceCoverage>,
}
