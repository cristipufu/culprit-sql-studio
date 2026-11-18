//! An explicit unqualified source reuses the common streamed snapshot's one native owner.
use super::streamed::SnapshotRecipe;
use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_text, text},
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, Source, telemetry::*};

pub(super) struct HyperscaleLogComponents;
#[cfg(test)]
pub(super) const STANDALONE_QUERY_COUNT: u32 = 8;
pub(super) const SOURCE_READ_QUERY_COUNT: u32 = 10;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    database_id: u32,
}

pub(super) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        max_snapshot_rows: MAX_HYPERSCALE_LOG_COMPONENT_ROWS,
        max_queries: SOURCE_READ_QUERY_COUNT,
        page_rows: 64,
        max_pages: 32,
        page_bytes: 256 * 1024,
        total_bytes: 2 * 1024 * 1024,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
        ..CollectionBudget::default()
    })
}

// Exact schema observed on the authorized HS metadata-only check; the DMF was not executed or cost-qualified.
// No aliases/fallbacks are guessed from the conflicting documentation table.
const LAYOUT: &str = r#"SELECT /* culprit */
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'Edition')) AS database_edition,
 CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),'Updateability')) AS updateability,
 DB_ID() AS database_id,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE') AS performance_visibility,
 (SELECT COUNT_BIG(*) FROM sys.all_columns AS c
  WHERE c.object_id=OBJECT_ID(N'sys.dm_hs_database_log_rate') AND
   ((c.name=N'database_id' AND c.system_type_id=56 AND c.is_nullable=0)
    OR(c.name=N'replica_id' AND c.system_type_id=36 AND c.is_nullable=0)
    OR(c.name=N'file_id' AND c.system_type_id=56 AND c.is_nullable=1)
    OR(c.name IN(N'current_max_log_rate_bps',N'catchup_rate_bps',N'catchup_distance_bytes') AND c.system_type_id=127 AND c.is_nullable=0)
    OR(c.name=N'component_id' AND c.system_type_id=36 AND c.is_nullable=0)
    OR(c.name=N'role' AND c.system_type_id=48 AND c.is_nullable=0)
    OR(c.name=N'role_desc' AND c.system_type_id=231 AND c.max_length=120 AND c.is_nullable=1)
    OR(c.name=N'last_reported_time' AND c.system_type_id=61 AND c.is_nullable=1))) AS supported_columns"#;
const COMPONENTS: &str = r#"SELECT TOP (@P1) /* culprit */ database_id,replica_id,file_id,
 current_max_log_rate_bps,component_id,role,role_desc,catchup_rate_bps,catchup_distance_bytes,
 CONVERT(varchar(23),last_reported_time,126) AS last_reported_time_without_offset
 FROM sys.dm_hs_database_log_rate(DB_ID())
 WHERE database_id=DB_ID()
 ORDER BY component_id"#;

impl Layout {
    async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let rows = reader.query(LAYOUT, vec![], 1).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        if integer(row, "edition")? != 5 || optional_text(row, "database_edition")?.as_deref() != Some("Hyperscale") {
            return Err(Error::new(
                ErrorCode::Unsupported,
                culprit_core::t!("view.hyperscale_log_components.unavailable"),
            ));
        }
        crate::read::require_primary(match optional_text(row, "updateability")?.as_deref() {
            Some("READ_WRITE") => Some(true),
            Some("READ_ONLY") => Some(false),
            _ => None,
        })?;
        if integer(row, "performance_visibility")? != 1 {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                culprit_core::t!("view.hyperscale_log_components.unavailable"),
            ));
        }
        if integer(row, "supported_columns")? != 10 {
            return Err(invalid_data());
        }
        let database_id = u32::try_from(integer(row, "database_id")?).map_err(|_| invalid_data())?;
        if database_id == 0 || database_id > i32::MAX as u32 {
            return Err(invalid_data());
        }
        Ok(Self { database_id })
    }
}
#[async_trait::async_trait]
impl SnapshotRecipe for HyperscaleLogComponents {
    type Layout = Layout;
    const INSTANCE_LOCAL: bool = true;
    const DATASET: SnapshotDataset = SnapshotDataset::HyperscaleLogComponents;
    const NAME: &'static str = HYPERSCALE_LOG_COMPONENTS_SOURCE;
    const SOURCE_READ: Option<SourceReadQuestion> = Some(SourceReadQuestion::HyperscaleLogComponents);
    const SOURCE: &'static Source = &crate::sources::HYPERSCALE_LOG_COMPONENTS;
    async fn layout<R: NativeRead>(&self, reader: &mut R, _: &CollectionBudget) -> Result<Option<Layout>> {
        Layout::read(reader).await.map(Some)
    }
    fn query(&self, _: &Layout) -> String {
        COMPONENTS.into()
    }
    fn decode(&self, row: &Rec, layout: &Layout, source: &CollectionDescription) -> Result<TelemetryRecord> {
        let database_id = integer(row, "database_id")?;
        if database_id != i64::from(layout.database_id) {
            return Err(invalid_data());
        }
        let point = HyperscaleLogComponent {
            database_id: database_id.to_string(),
            database_incarnation: source
                .target
                .database()
                .and_then(|database| database.incarnation.clone())
                .ok_or_else(invalid_data)?,
            replica_id: optional_text(row, "replica_id")?,
            file_id: crate::read::optional_counter(row, "file_id")?,
            component_id: text(row, "component_id")?,
            role: u8::try_from(integer(row, "role")?).map_err(|_| invalid_data())?,
            role_desc: optional_text(row, "role_desc")?,
            current_max_log_rate_bps: integer(row, "current_max_log_rate_bps")?.to_string(),
            catchup_rate_bps: integer(row, "catchup_rate_bps")?.to_string(),
            catchup_distance_bytes: integer(row, "catchup_distance_bytes")?.to_string(),
            last_reported_time_without_offset: optional_text(row, "last_reported_time_without_offset")?,
        };
        if !point.fits_source(source) {
            return Err(invalid_data());
        }
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: point.key()?,
            fact: OperationalFact::HyperscaleLogComponent(Box::new(point)),
        }))
    }
    async fn recheck<R: NativeRead>(&self, reader: &mut R, layout: &Layout) -> Result<()> {
        if &Layout::read(reader).await? != layout {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(())
    }
}
#[cfg(test)]
pub(super) mod tests;
