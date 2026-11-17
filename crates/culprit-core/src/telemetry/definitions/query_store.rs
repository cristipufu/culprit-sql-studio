//! Query Store native incarnation and logical namespace policy, independent of counter lifetimes.
use super::super::*;

/// Query Store's logical namespace needs no invented acquisition context from a runtime interval.
pub fn query_store_database_namespace(target: &CollectionTarget, partition: &SourcePartition) -> Option<String> {
    let database = target.database()?;
    (*partition == SourcePartition::Target
        && database.engine == crate::Engine::Mssql
        && database.incarnation.as_ref().is_some_and(|id| !id.is_empty()))
    .then(|| target.telemetry_id().ok())
    .flatten()
}

/// A complete source-owned incarnation is required; a query number or hash alone is never a statement link.
pub fn query_store_query_identity_is_complete(identity: &NativeQueryIdentity) -> bool {
    identity.is_valid()
        && valid_counter(&identity.query_id)
        && identity.qualifiers.get("context_settings_id").is_some_and(|value| valid_counter(value))
        && identity.qualifiers.get("object_id").is_some_and(|value| value.parse::<i64>().is_ok())
        && identity
            .qualifiers
            .get("query_hash")
            .is_some_and(|value| value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && identity
            .qualifiers
            .get("initial_compile_at")
            .is_some_and(|value| value.len() <= 64 && canonical_time(value).is_ok())
}

/// Canonical comparison of the complete native query incarnation; this supplies no source authentication.
pub fn query_store_query_incarnation_key(identity: &NativeQueryIdentity) -> Result<String> {
    if !query_store_query_identity_is_complete(identity) {
        return Err(CollectionStop::InvalidPage.error());
    }
    let mut canonical = identity.clone();
    let initial = canonical_time(
        canonical.qualifiers.get("initial_compile_at").ok_or_else(|| CollectionStop::InvalidPage.error())?,
    )?;
    canonical.qualifiers.insert("initial_compile_at".into(), initial);
    serde_json::to_vec(&canonical).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
}
