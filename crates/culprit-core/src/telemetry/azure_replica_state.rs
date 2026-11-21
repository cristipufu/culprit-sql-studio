//! Azure local database-replica state is neither AG topology nor a complete read-scale roster.
use super::*;
pub const MSSQL_AZURE_REPLICA_STATE_SOURCE: &str = "azure-local-database-replica-state";
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAzureReplicaState {
    pub database_id: u32,
    pub replica_id: String,
    pub is_local: bool,
    pub database_writable: bool,
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub group_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub group_database_id: Option<String>,
    pub progress: MssqlReplicaDatabaseProgress,
}
impl MssqlAzureReplicaState {
    pub fn is_valid(&self) -> bool {
        self.database_id > 0
            && self.database_id <= i32::MAX as u32
            && super::replication::native_guid(&self.replica_id)
            && self.replica_id.bytes().any(|byte| byte != b'0' && byte != b'-')
            && self.is_local
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && [&self.group_id, &self.group_database_id]
                .into_iter()
                .flatten()
                .all(|id| super::replication::native_guid(id))
            && self.progress.mode.is_none()
            && self.progress.native_mode.is_none()
            && self.progress.is_valid()
    }
    pub fn fits_read(&self, scope: &MssqlReplicationScope, context: &NativeReadContext) -> bool {
        self.is_valid()
            && scope.is_valid_for(context)
            && self.database_id == scope.database_id
            && self.replica_id.eq_ignore_ascii_case(&scope.replica_id)
            && Some(self.database_writable) == context.database_writable
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }
}
#[cfg(test)]
mod tests;
