//! Cache definitions retain native compilation identity, raw-content fingerprints and explicit payload availability.
use super::*;
const MAX_CACHED_TEXT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCachedPayload {
    /// SQL Server DATALENGTH before redaction, in UTF-16 bytes. No value is inferred for a missing native body.
    pub source_utf16_bytes: Option<String>,
    /// SHA-256 of the original decoded UTF-8 text, independent of redaction and transport representation.
    pub native_sha256: Option<String>,
    pub payload: DefinitionPayload,
}
impl MssqlCachedPayload {
    pub fn is_valid(&self) -> bool {
        self.source_utf16_bytes.as_deref().is_none_or(valid_counter)
            && self.native_sha256.as_deref().is_none_or(valid_digest)
            && self.payload.is_valid()
            && (!self.payload.is_available() || self.source_utf16_bytes.is_some() && self.native_sha256.is_some())
            && match &self.payload {
                DefinitionPayload::Available { text, .. } => text.len() <= MAX_CACHED_TEXT_BYTES,
                _ => true,
            }
    }
    fn redact(&mut self, engine: crate::Engine, redact: fn(&str, crate::Engine) -> DefinitionPayload) {
        if let DefinitionPayload::Available { text, .. } = &self.payload {
            let payload = redact(text, engine);
            self.payload = if matches!(&payload,DefinitionPayload::Available {text,..} if text.len()>MAX_CACHED_TEXT_BYTES)
            {
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
            } else {
                payload
            };
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCachedDefinition {
    pub context: CachedStatementContext,
    pub query_plan_hash: Option<String>,
    pub statement: MssqlCachedPayload,
    /// Estimated XML for the native statement offsets, not an arbitrary statement from the batch.
    pub estimated_plan: MssqlCachedPayload,
}
impl MssqlCachedDefinition {
    pub fn is_valid(&self) -> bool {
        self.context.is_valid()
            && self.query_plan_hash.as_deref().is_none_or(super::cached_statements::native_query_hash)
            && self.statement.is_valid()
            && self.estimated_plan.is_valid()
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        self.statement.redact(engine, DefinitionPayload::sql);
        self.estimated_plan.redact(engine, DefinitionPayload::plan);
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlRetainedPlanDefinition {
    pub context: CachedStatementContext,
    /// Native statement-cache counter; it is not the number of measured executions in the XML batch.
    pub statement_executions: Option<String>,
    pub last_start_without_offset: Option<String>,
    /// Database-scoped LAST_QUERY_PLAN_STATS only; server trace flags may enable retention independently.
    pub database_last_query_plan_stats: Option<bool>,
    /// Last-known native batch XML may be simplified or lack runtime measurements. Nothing is executed to obtain it.
    pub batch_plan: MssqlCachedPayload,
}
impl MssqlRetainedPlanDefinition {
    pub fn is_valid(&self) -> bool {
        self.context.is_valid()
            && self.statement_executions.as_deref().is_none_or(valid_counter)
            && self.statement_executions.is_some() == self.context.identity.has_plan_handle()
            && self.last_start_without_offset.as_deref().is_none_or(super::catalog::local_time)
            && self.batch_plan.is_valid()
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        self.batch_plan.redact(engine, DefinitionPayload::plan);
    }
}
