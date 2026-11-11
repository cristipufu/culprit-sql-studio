//! Versioned native catalog facts are definitions, independent of counters and rendered messages.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlObjectIdentity {
    pub database_id: u32,
    pub object_id: u32,
    /// Native SQL Server datetime has no offset. No UTC instant is inferred from it.
    pub created_without_offset: String,
}
impl MssqlObjectIdentity {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub(super) fn is_valid(&self) -> bool {
        self.database_id > 0 && self.object_id > 0 && local_time(&self.created_without_offset)
    }
}
pub(super) fn local_time(at: &str) -> bool {
    at.len() <= 64 && chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
}

pub const MAX_CATALOG_EXPRESSION_BYTES: usize = 16 * 1024;

/// A bounded catalog expression retains its native version independently of redacted literals.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CatalogExpression {
    pub native_sha256: Option<String>,
    pub payload: DefinitionPayload,
}
impl CatalogExpression {
    pub fn sql(raw: &str, engine: crate::Engine, maximum_bytes: usize) -> Self {
        Self::new(raw, engine, maximum_bytes, DefinitionPayload::sql)
    }
    pub fn partition_bound(raw: &str, engine: crate::Engine, maximum_bytes: usize) -> Self {
        Self::new(raw, engine, maximum_bytes, DefinitionPayload::partition_bound)
    }
    fn new(
        raw: &str,
        engine: crate::Engine,
        maximum_bytes: usize,
        redact: fn(&str, crate::Engine) -> DefinitionPayload,
    ) -> Self {
        let maximum_bytes = maximum_bytes.min(MAX_CATALOG_EXPRESSION_BYTES);
        if raw.len() > maximum_bytes {
            return Self {
                native_sha256: None,
                payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized },
            };
        }
        Self {
            native_sha256: Some(digest(raw.as_bytes())),
            payload: Self::bounded_payload(raw, engine, maximum_bytes, redact),
        }
    }
    fn bounded_payload(
        raw: &str,
        engine: crate::Engine,
        maximum_bytes: usize,
        redact: fn(&str, crate::Engine) -> DefinitionPayload,
    ) -> DefinitionPayload {
        let payload = redact(raw, engine);
        if matches!(&payload,DefinitionPayload::Available { text,.. } if text.len()>maximum_bytes) {
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
        } else {
            payload
        }
    }
    pub fn is_valid(&self) -> bool {
        self.native_sha256.as_deref().is_none_or(valid_digest)
            && self.payload.is_valid()
            && match &self.payload {
                DefinitionPayload::Available { text, .. } => {
                    self.native_sha256.is_some() && text.len() <= MAX_CATALOG_EXPRESSION_BYTES
                }
                DefinitionPayload::Unavailable { .. } => true,
            }
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        if let DefinitionPayload::Available { text, .. } = &self.payload {
            self.payload = Self::bounded_payload(text, engine, MAX_CATALOG_EXPRESSION_BYTES, DefinitionPayload::sql);
        }
    }
    pub(super) fn redact_partition_bound(&mut self, engine: crate::Engine) {
        if let DefinitionPayload::Available { text, .. } = &self.payload {
            self.payload =
                Self::bounded_payload(text, engine, MAX_CATALOG_EXPRESSION_BYTES, DefinitionPayload::partition_bound);
        }
    }
}

/// Exact native catalog attributes. SQL is retained only in the inventoried expression fields.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeCatalogDefinition {
    pub identity: NativeIdentity,
    pub object_type: String,
    pub properties: std::collections::BTreeMap<String, String>,
    pub definition: Option<CatalogExpression>,
    pub predicate: Option<CatalogExpression>,
}
impl NativeCatalogDefinition {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && valid_key(&self.object_type)
            && self.properties.len() <= 64
            && self.properties.iter().all(|(key, value)| valid_key(key) && value.len() <= 16 * 1024)
            && [&self.definition, &self.predicate].into_iter().flatten().all(|value| value.is_valid())
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        for expression in [&mut self.definition, &mut self.predicate].into_iter().flatten() {
            expression.redact(engine);
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeCatalogCounters {
    pub catalog: NativeCatalogDefinition,
    pub counters: NativeCounterSet,
}
impl NativeCatalogCounters {
    pub fn is_valid(&self) -> bool {
        self.catalog.is_valid() && self.counters.is_valid() && self.catalog.identity == self.counters.identity
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum ObjectDefinitionFact {
    Statement(Box<QueryDefinition>),
    RetainedPlan(Box<NativeRetainedPlan>),
    NativeCatalog(Box<NativeCatalogDefinition>),
    MssqlForeignKey(Box<MssqlForeignKey>),
    MssqlColumn(Box<MssqlColumnDefinition>),
    MssqlCheckConstraint(Box<MssqlCheckConstraint>),
    MssqlObject(Box<MssqlCatalogObject>),
    MssqlDmlTrigger(Box<MssqlDmlTrigger>),
    MssqlPartitionBoundary(Box<MssqlPartitionBoundary>),
    MssqlCachedDefinition(Box<MssqlCachedDefinition>),
    MssqlRetainedPlan(Box<MssqlRetainedPlanDefinition>),
}
impl ObjectDefinitionFact {
    pub fn key(&self) -> Result<String> {
        match self {
            Self::NativeCatalog(catalog) => catalog.key(),
            Self::RetainedPlan(plan) => Ok(plan.definition.id.clone()),
            Self::Statement(statement) => serde_json::to_vec(&statement.identity)
                .map(|bytes| digest(&bytes))
                .map_err(|_| CollectionStop::InvalidPage.error()),
            Self::MssqlForeignKey(key) => key.identity.key(),
            Self::MssqlColumn(column) => column.key(),
            Self::MssqlCheckConstraint(constraint) => constraint.identity.key(),
            Self::MssqlObject(object) => object.identity.key(),
            Self::MssqlDmlTrigger(trigger) => trigger.identity.key(),
            Self::MssqlPartitionBoundary(boundary) => boundary.key(),
            Self::MssqlCachedDefinition(definition) => definition.context.identity.key(),
            Self::MssqlRetainedPlan(definition) => definition.context.identity.key(),
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::NativeCatalog(catalog) => catalog.is_valid(),
            Self::RetainedPlan(plan) => plan.is_valid(),
            Self::Statement(statement) => statement.is_valid(),
            Self::MssqlForeignKey(key) => key.is_valid(),
            Self::MssqlColumn(column) => column.is_valid(),
            Self::MssqlCheckConstraint(constraint) => constraint.is_valid(),
            Self::MssqlObject(object) => object.is_valid(),
            Self::MssqlDmlTrigger(trigger) => trigger.is_valid(),
            Self::MssqlPartitionBoundary(boundary) => boundary.is_valid(),
            Self::MssqlCachedDefinition(definition) => definition.is_valid(),
            Self::MssqlRetainedPlan(definition) => definition.is_valid(),
        }
    }
    fn redact(&mut self, engine: crate::Engine) {
        match self {
            Self::NativeCatalog(catalog) => catalog.redact(engine),
            Self::RetainedPlan(plan) => plan.redact(engine),
            Self::Statement(statement) => {
                if let DefinitionPayload::Available { text, .. } = &statement.sql {
                    statement.sql = DefinitionPayload::sql(text, engine);
                }
            }
            Self::MssqlForeignKey(_) | Self::MssqlObject(_) | Self::MssqlDmlTrigger(_) => {}
            Self::MssqlCheckConstraint(constraint) => constraint.definition.redact(engine),
            Self::MssqlCachedDefinition(definition) => definition.redact(engine),
            Self::MssqlRetainedPlan(definition) => definition.redact(engine),
            Self::MssqlPartitionBoundary(boundary) => {
                if let Some(value) = &mut boundary.display_value {
                    value.redact_partition_bound(engine)
                }
            }
            Self::MssqlColumn(column) => {
                for expression in [&mut column.default, &mut column.computed].into_iter().flatten() {
                    expression.redact(engine);
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ObjectDefinitionRecord {
    pub source: String,
    pub key: String,
    /// Hash of the typed definition, including its native generation and properties.
    pub version: String,
    pub fact: ObjectDefinitionFact,
}
impl ObjectDefinitionRecord {
    pub(super) fn for_export(&self, engine: crate::Engine) -> Result<Self> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut fact = self.fact.clone();
        fact.redact(engine);
        Self::new(self.source.clone(), fact)
    }
    pub fn new(source: String, fact: ObjectDefinitionFact) -> Result<Self> {
        let key = fact.key()?;
        let version = digest(&serde_json::to_vec(&fact).map_err(|_| CollectionStop::InvalidPage.error())?);
        let record = Self { source, key, version, fact };
        if !record.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(record)
    }
    pub fn is_valid(&self) -> bool {
        valid_key(&self.source)
            && valid_digest(&self.key)
            && valid_digest(&self.version)
            && self.fact.is_valid()
            && self.fact.key().is_ok_and(|key| key == self.key)
            && serde_json::to_vec(&self.fact).is_ok_and(|bytes| digest(&bytes) == self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_expressions_keep_native_versions_and_bound_bodies() {
        let first = CatalogExpression::sql("(N'first-secret')", crate::Engine::Mssql, MAX_CATALOG_EXPRESSION_BYTES);
        let second = CatalogExpression::sql("(N'second-secret')", crate::Engine::Mssql, MAX_CATALOG_EXPRESSION_BYTES);
        assert!(first.is_valid() && second.is_valid());
        assert_eq!(first.payload, second.payload);
        assert_ne!(first.native_sha256, second.native_sha256);
        assert!(!serde_json::to_string(&first).unwrap().contains("first-secret"));
        let oversized = CatalogExpression::sql(
            &"x".repeat(MAX_CATALOG_EXPRESSION_BYTES + 1),
            crate::Engine::Mssql,
            MAX_CATALOG_EXPRESSION_BYTES,
        );
        assert!(oversized.is_valid());
        assert!(matches!(oversized.payload, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }));
    }
    #[test]
    fn redaction_growth_cannot_exceed_the_requested_or_absolute_body_limit() {
        for limit in [MAX_CATALOG_EXPRESSION_BYTES, 1024] {
            let raw = "''+".repeat((limit - 2) / 3) + "''";
            assert!(raw.len() <= limit);
            let expression = CatalogExpression::sql(&raw, crate::Engine::Mssql, limit);
            assert!(expression.is_valid());
            assert!(matches!(
                expression.payload,
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
            ));
        }
    }
    #[test]
    fn object_identity_preserves_generation_without_accepting_a_fabricated_offset() {
        let first = MssqlObjectIdentity {
            database_id: 5,
            object_id: 100,
            created_without_offset: "2026-11-10T07:00:00.123".into(),
        };
        assert!(first.is_valid());
        let mut recreated = first.clone();
        recreated.created_without_offset = "2026-11-10T07:01:00.123".into();
        assert_ne!(first.key().unwrap(), recreated.key().unwrap());
        recreated.created_without_offset.push('Z');
        assert!(!recreated.is_valid());
    }
}
