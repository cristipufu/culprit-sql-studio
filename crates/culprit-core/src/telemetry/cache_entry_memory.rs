//! Whole native cache objects are independent of statement execution, grant and compilation memory.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CACHE_ENTRY_STATEMENTS: usize = 8;
pub const MAX_CACHE_KEY_ATTRIBUTES: usize = 64;
pub const MAX_CACHE_KEY_VALUE_BYTES: usize = 512;

/// Exact already-observed statements select a bounded cache investigation. No hash discovers another entry.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CacheEntryMemoryFocus {
    pub context: NativeReadContext,
    pub statements: Vec<CachedStatementContext>,
}

impl CacheEntryMemoryFocus {
    pub fn is_valid(&self) -> bool {
        let mut statements = BTreeSet::new();
        let mut entries = BTreeMap::new();
        self.context.has_qualified_local_owner()
            && matches!(self.context.target, CollectionTarget::Database(_))
            && self.context.target.engine() == crate::Engine::Mssql
            && !self.statements.is_empty()
            && self.statements.len() <= MAX_CACHE_ENTRY_STATEMENTS
            && self.statements.iter().all(|statement| {
                statement.is_valid()
                    && statement.identity.has_plan_handle()
                    && statement.identity.has_sql_handle()
                    && statement.database_id <= i32::MAX as u32
                    && statement.identity.statement_start_bytes <= i32::MAX as u32
                    && statement.identity.plan_generation.parse::<i64>().is_ok()
                    && statement.database_id == self.statements[0].database_id
                    && statement.identity.key().is_ok_and(|key| statements.insert(key))
                    && match entries.insert(
                        statement.identity.plan_handle.to_ascii_lowercase(),
                        statement.identity.sql_handle.to_ascii_lowercase(),
                    ) {
                        Some(previous) => previous.eq_ignore_ascii_case(&statement.identity.sql_handle),
                        None => true,
                    }
            })
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut statements = self
            .statements
            .iter()
            .map(|statement| statement.identity.key().map(|key| (key, statement)))
            .collect::<Result<Vec<_>>>()?;
        statements.sort_by(|left, right| left.0.cmp(&right.0));
        serde_json::to_vec(&(&self.context.target, &self.context.partition, self.context.database_writable, statements))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    /// The enclosing source must separately bind this focus's target/member/epoch. Within that source, only
    /// exact original statement incarnations are admissible. Newly measured text availability/provenance and a
    /// previously unknown query hash are current observations; they never rewrite this retained original focus.
    pub fn accepts_entry(&self, entry: &MssqlCacheEntryMemory) -> bool {
        self.is_valid()
            && entry.is_valid()
            && ObservationBounds::new(&self.context.observed_until, &entry.observed_at).is_some()
            && entry.statements.iter().all(|current| {
                self.statements.iter().any(|original| {
                    original.identity.key().ok() == current.context.identity.key().ok()
                        && original.database_id == current.context.database_id
                        && original.source_text_bytes.as_deref().is_none_or(|bytes| {
                            current
                                .context
                                .source_text_bytes
                                .as_deref()
                                .is_some_and(|current| bytes.parse::<u64>().ok() == current.parse::<u64>().ok())
                        })
                        && original.query_hash.as_deref().is_none_or(|hash| {
                            current
                                .context
                                .query_hash
                                .as_deref()
                                .is_some_and(|current| hash.eq_ignore_ascii_case(current))
                        })
                })
            })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CacheEntryStatement {
    pub context: CachedStatementContext,
    pub statement: DefinitionPayload,
}

impl CacheEntryStatement {
    pub fn is_valid(&self) -> bool {
        self.context.is_valid()
            && self.context.identity.has_plan_handle()
            && self.context.identity.has_sql_handle()
            && self.statement.is_valid()
            && (!self.statement.is_available()
                || self.context.source_text_complete
                    && self
                        .context
                        .source_text_bytes
                        .as_deref()
                        .is_some_and(|bytes| bytes.parse::<u64>().is_ok_and(|bytes| bytes > 0)))
    }
}

/// Only attributes explicitly marked is_cache_key by the native source are admitted here. Unknown scalar
/// representations remain None; an absent value never establishes equality or a context difference.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CacheKeyAttribute {
    pub name: String,
    pub native_type: Option<String>,
    pub value: Option<String>,
}

impl CacheKeyAttribute {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.name)
            && self.name.len() <= 128
            && self.native_type.as_deref().is_none_or(|kind| valid_key(kind) && kind.len() <= 128)
            && (self.value.is_none() || self.native_type.is_some())
            && self
                .value
                .as_deref()
                .is_none_or(|value| value.len() <= MAX_CACHE_KEY_VALUE_BYTES && !value.contains('\0'))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCacheEntryMemory {
    pub observed_at: String,
    pub plan_handle: String,
    pub sql_handle: String,
    /// sys.dm_exec_cached_plans.size_in_bytes for the whole object, counted once for all selected statements.
    pub size_bytes: String,
    pub cache_object_type: String,
    pub object_type: String,
    /// Native cache lookups. Not executions, compilations or a count of successful plan reuse.
    pub lookup_count: String,
    pub reference_count: String,
    pub cache_keys: Vec<CacheKeyAttribute>,
    pub statements: Vec<CacheEntryStatement>,
}

impl MssqlCacheEntryMemory {
    pub fn is_valid(&self) -> bool {
        let mut statements = BTreeSet::new();
        let mut attributes = BTreeSet::new();
        canonical_time(&self.observed_at).is_ok()
            && valid_counter(&self.size_bytes)
            && valid_counter(&self.lookup_count)
            && valid_counter(&self.reference_count)
            && valid_key(&self.cache_object_type)
            && valid_key(&self.object_type)
            && self.cache_keys.len() <= MAX_CACHE_KEY_ATTRIBUTES
            && self.cache_keys.iter().all(|attribute| attribute.is_valid() && attributes.insert(&attribute.name))
            && !self.statements.is_empty()
            && self.statements.len() <= MAX_CACHE_ENTRY_STATEMENTS
            && self.statements.iter().all(|statement| {
                statement.is_valid()
                    && statement.context.identity.plan_handle.eq_ignore_ascii_case(&self.plan_handle)
                    && statement.context.identity.sql_handle.eq_ignore_ascii_case(&self.sql_handle)
                    && statement.context.database_id == self.statements[0].context.database_id
                    && statement.context.identity.key().is_ok_and(|key| statements.insert(key))
            })
    }

    /// A whole-entry point reference. Handles do not authorize cross-capture continuity or a counter delta.
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let point = canonical_time(&self.observed_at)?;
        serde_json::to_vec(&(point, self.plan_handle.to_ascii_lowercase(), self.sql_handle.to_ascii_lowercase()))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub(super) fn redact(&mut self, engine: crate::Engine) {
        for statement in &mut self.statements {
            if let DefinitionPayload::Available { text, .. } = &statement.statement {
                statement.statement = DefinitionPayload::sql(text, engine);
            }
        }
    }
}

#[cfg(test)]
mod tests;
