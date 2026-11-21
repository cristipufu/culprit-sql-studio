//! Original current-pool points reuse the native membership projection, never catalog/DMV ID joins.
use super::*;
use crate::read::{integer, optional_boolean};
// Azure SQL documents this effective-permission query in the connected user database; a missing row stays NULL.
// https://learn.microsoft.com/en-us/azure/azure-sql/database/security-server-roles#c-complete-example-add-a-login-to-a-server-level-role-retrieve-metadata-for-role-membership-and-permissions-and-run-a-test-query
// https://learn.microsoft.com/en-us/sql/relational-databases/system-functions/sys-fn-my-permissions-transact-sql
const EXTRA: &str = ",DB_ID() AS selected_database_id,g.database_id,g.database_name,CONVERT(int,SERVERPROPERTY('EngineEdition')) AS pool_engine_edition,CONVERT(nvarchar(64),DATABASEPROPERTYEX(DB_NAME(),'Edition')) AS native_tier,(SELECT TOP (1) CAST(1 AS int) FROM sys.fn_my_permissions(NULL,N'SERVER') granted_permission WHERE granted_permission.permission_name=N'VIEW SERVER PERFORMANCE STATE') AS server_performance_state,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE',N'VIEW DEFINITION') AS metadata_visibility";
#[derive(Clone)]
pub(crate) struct CurrentContext {
    pub point: MssqlPoolCurrentPoint,
    generation: u64,
    pub metadata_visible: bool,
}
impl CurrentContext {
    pub(crate) async fn read(reader: &mut impl NativeRead) -> Result<Self> {
        let generation = reader
            .connection_generation()
            .filter(|generation| *generation > 0)
            .ok_or_else(|| CollectionStop::SourceUnavailable.error())?;
        let row = one(reader, &identity_query(EXTRA)).await?;
        let (membership, at) = decode_membership(&row)?;
        let pool = membership.pool.ok_or_else(|| CollectionStop::SourceChanged.error())?;
        let database_id = u32::try_from(integer(&row, "database_id")?).map_err(|_| invalid_data())?;
        if i64::from(database_id) != integer(&row, "selected_database_id")?
            || reader.connection_generation() != Some(generation)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let point = MssqlPoolCurrentPoint {
            pool: pool.identity,
            member: membership.route,
            database_id,
            database_name: text(&row, "database_name")?,
            engine_edition: u32::try_from(integer(&row, "pool_engine_edition")?).map_err(|_| invalid_data())?,
            native_tier: crate::read::optional_text(&row, "native_tier")?,
            server_performance_state: optional_boolean(&row, "server_performance_state")?,
            database_writable: true,
            observed_at: at,
        };
        if !point.is_valid() {
            return Err(invalid_data());
        }
        Ok(Self { point, generation, metadata_visible: integer(&row, "metadata_visibility")? == 1 })
    }
    pub(crate) async fn recheck(&self, reader: &mut impl NativeRead) -> Result<Self> {
        if reader.connection_generation() != Some(self.generation) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Self::read(reader).await
    }
    pub(crate) fn confirms(&self, after: &Self, last_at: &str) -> bool {
        self.generation == after.generation
            && self.point.same_membership(&after.point)
            && ObservationBounds::new(last_at, &after.point.observed_at).is_some()
    }
}

impl PartialEq for CurrentContext {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.point.same_membership(&other.point)
    }
}
impl Eq for CurrentContext {}
