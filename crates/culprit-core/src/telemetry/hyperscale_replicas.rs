//! Selected native Hyperscale topology. Logical labels and parent edges establish no compute/work owner.
use super::*;

pub const MSSQL_HYPERSCALE_REPLICAS_SOURCE: &str = "hyperscale-replica-topology";

/// Original held-reader authority. This is never constructed from a returned topology row or a display name.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlReplicationScope {
    pub database_id: u32,
    pub replica_id: String,
}
impl MssqlReplicationScope {
    pub fn is_valid(&self) -> bool {
        self.database_id > 0 && self.database_id <= i32::MAX as u32 && replica_id(&self.replica_id)
    }
    pub fn is_valid_for(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.has_qualified_local_owner()
            && context.database_writable.is_some()
            && context.target.engine() == crate::Engine::Mssql
            && context.target.database().is_some()
            && matches!(&context.partition,
                SourcePartition::InstanceLocal { member: Some(member), .. }
                    if member.kind == SharedResourceKind::Node
                        && member.native.id.eq_ignore_ascii_case(&self.replica_id)
                        && member.native.qualifiers.get("kind").map(String::as_str) == Some("azure-sql-hyperscale-replica")
                        && member.namespace.qualifiers.get("kind").map(String::as_str) == Some("azure-sql-logical-database")
                        && context.target.database().and_then(|database| database.incarnation.as_deref()) == Some(member.namespace.id.as_str()))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlHyperscaleReplica {
    /// Same-batch native witnesses, checked against the independent original header before publication.
    pub selected_database_id: u32,
    pub observer_replica_id: String,
    pub database_writable: bool,
    pub observed_at: String,
    pub replica_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub replica_server_name: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub replica_database_name: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub is_local: Option<bool>,
    /// Native 0 primary, 1 HA secondary, 2 geo-forwarder, 3 named. Other/NULL remains unknown.
    #[serde(deserialize_with = "Option::deserialize")]
    pub replica_role: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_role: Option<String>,
    /// Native DB/pool-local identifier of the reported replica; remote values are not this observer's DBID.
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_database_id: Option<u32>,
    /// A raw HA parent edge, not inferred from a role, name, availability-group row or current catalog.
    #[serde(deserialize_with = "Option::deserialize")]
    pub parent_replica_id: Option<String>,
}
impl MssqlHyperscaleReplica {
    pub fn is_valid(&self) -> bool {
        use super::replication::native_name;
        self.selected_database_id > 0
            && self.selected_database_id <= i32::MAX as u32
            && replica_id(&self.observer_replica_id)
            && replica_id(&self.replica_id)
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && [&self.replica_server_name, &self.replica_database_name]
                .into_iter()
                .flatten()
                .all(|value| native_name(value) && value.encode_utf16().count() <= 128)
            && self.native_role.as_deref().is_none_or(|value| native_name(value) && value.encode_utf16().count() <= 256)
            && self.local_database_id.is_none_or(|id| id <= i32::MAX as u32)
            && self.parent_replica_id.as_deref().is_none_or(replica_id)
            && self.parent_replica_id.as_deref().is_none_or(|parent| !parent.eq_ignore_ascii_case(&self.replica_id))
            && (!matches!(self.replica_role, Some(0 | 2 | 3)) || self.parent_replica_id.is_none())
            && self
                .is_local
                .is_none_or(|local| local == self.replica_id.eq_ignore_ascii_case(&self.observer_replica_id))
            && (self.is_local != Some(true)
                || (self.replica_id.eq_ignore_ascii_case(&self.observer_replica_id)
                    && self.local_database_id.is_none_or(|id| id == self.selected_database_id)))
    }
    pub fn fits_read(&self, scope: &MssqlReplicationScope, context: &NativeReadContext) -> bool {
        self.is_valid()
            && scope.is_valid_for(context)
            && self.selected_database_id == scope.database_id
            && self.observer_replica_id.eq_ignore_ascii_case(&scope.replica_id)
            && Some(self.database_writable) == context.database_writable
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }
}

fn replica_id(value: &str) -> bool {
    super::replication::native_guid(value) && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}

/// Validate the specialized header without reconstructing authority from any body or recursive source lookup.
pub fn mssql_replication_scope(source: &CollectionDescription) -> Result<Option<&MssqlReplicationScope>> {
    let SourceObservation::MssqlReplication { snapshot, scope, database_writable } = &source.observation else {
        return Ok(None);
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if snapshot.dataset != SnapshotDataset::Replication || !snapshot.is_valid() || !scope.is_valid_for(&context) {
        return Err(CollectionStop::InvalidPage.error());
    }
    if snapshot.sources.iter().any(|source| {
        matches!(source.source.as_str(), MSSQL_HYPERSCALE_REPLICAS_SOURCE | MSSQL_AZURE_REPLICA_STATE_SOURCE)
            && source.complete_scope
    }) {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(Some(scope))
}

#[cfg(test)]
pub(super) mod tests;
