//! One selected DMV metadata row, never a hash-bucket, allocator or application-row walk.
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_counter, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetPoint {
    pub metadata: MssqlTableMemoryMetadata,
    modified: String,
    writable: bool,
}
impl TargetPoint {
    pub fn same_object_state(&self, other: &Self) -> bool {
        self.modified == other.modified
            && self.writable == other.writable
            && self.metadata.schema == other.metadata.schema
            && self.metadata.name == other.metadata.name
            && self.metadata.memory_optimized == other.metadata.memory_optimized
            && self.metadata.xtp_supported == other.metadata.xtp_supported
            && self.metadata.dmv_permission == other.metadata.dmv_permission
            && self.metadata.metadata_visible == other.metadata.metadata_visible
            && self.metadata.layout_available == other.metadata.layout_available
    }
    pub fn writable(&self) -> bool {
        self.writable
    }
}

fn query(permission: &'static str, memory: bool) -> String {
    let (fields, join) = if memory {
        (
            "CASE WHEN m.object_id IS NULL THEN 0 ELSE 1 END AS row_present,m.memory_allocated_for_table_kb,m.memory_used_by_table_kb,m.memory_allocated_for_indexes_kb,m.memory_used_by_indexes_kb,",
            "LEFT JOIN sys.dm_db_xtp_table_memory_stats m ON m.object_id=t.object_id",
        )
    } else {
        ("", "")
    };
    format!("DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
DECLARE /* culprit */ @updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability'));
SELECT TOP(2) /* culprit */ @observed_at AS observed_at,@updateability AS updateability,DB_ID() AS database_id,
 t.object_id,CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,s.name AS schema_name,t.name AS table_name,
 t.is_memory_optimized,CONVERT(int,SERVERPROPERTY('IsXTPSupported')) AS xtp_supported,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE',N'{permission}') AS dmv_permission,
 HAS_PERMS_BY_NAME(QUOTENAME(s.name)+N'.'+QUOTENAME(t.name),'OBJECT',N'VIEW DEFINITION') AS table_visibility,
 {fields}CASE WHEN (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_db_xtp_table_memory_stats')
  AND ((name=N'object_id' AND system_type_id=56) OR
       (name IN(N'memory_allocated_for_table_kb',N'memory_used_by_table_kb',N'memory_allocated_for_indexes_kb',N'memory_used_by_indexes_kb') AND system_type_id=127)))=5 THEN 1 ELSE 0 END AS layout_available
 FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id {join}
 WHERE t.object_id=@P1 AND t.is_ms_shipped=0 AND t.type=N'U' OPTION(MAXDOP 1)")
}
pub(crate) fn permission(major: u32) -> &'static str {
    if major >= 16 { "VIEW DATABASE PERFORMANCE STATE" } else { "VIEW DATABASE STATE" }
}
pub(crate) async fn target(
    reader: &mut impl NativeRead,
    target: &MssqlObjectIdentity,
    permission: &'static str,
) -> Result<TargetPoint> {
    let rows = reader
        .query(&query(permission, false), vec![Parameter::Integer(i64::from(target.object_id))], 2)
        .await
        .map_err(ReadError::into_error)?;
    let row = selected_row(&rows)?;
    decode_target(row, target)
}
fn selected_row(rows: &[Rec]) -> Result<&Rec> {
    match rows {
        [] => Err(CollectionStop::SourceUnavailable.error()),
        [row] => Ok(row),
        _ => Err(invalid_data()),
    }
}
fn decode_target(row: &Rec, target: &MssqlObjectIdentity) -> Result<TargetPoint> {
    if integer(row, "database_id")? != i64::from(target.database_id)
        || integer(row, "object_id")? != i64::from(target.object_id)
        || crate::sql_time::native_time(&text(row, "created_without_offset")?)?
            != crate::sql_time::native_time(&target.created_without_offset)?
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let metadata = MssqlTableMemoryMetadata {
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        schema: text(row, "schema_name")?,
        name: text(row, "table_name")?,
        memory_optimized: optional_boolean(row, "is_memory_optimized")?,
        xtp_supported: optional_boolean(row, "xtp_supported")?,
        dmv_permission: optional_boolean(row, "dmv_permission")?,
        metadata_visible: optional_boolean(row, "table_visibility")?,
        layout_available: optional_boolean(row, "layout_available")?.ok_or_else(invalid_data)?,
    };
    if !metadata.is_valid() {
        return Err(invalid_data());
    }
    Ok(TargetPoint {
        metadata,
        modified: crate::sql_time::native_time(&text(row, "modified_without_offset")?)?,
        writable: crate::read::database_writable(row)?.ok_or_else(invalid_data)?,
    })
}

pub(crate) struct Read {
    pub selected: TargetPoint,
    pub point: Option<MssqlTableMemory>,
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    target: &MssqlObjectIdentity,
    original: &TargetPoint,
    permission: &'static str,
) -> Result<Read> {
    if !original.metadata.available() {
        return Err(invalid_data());
    }
    let rows = reader
        .query(&query(permission, true), vec![Parameter::Integer(i64::from(target.object_id))], 2)
        .await
        .map_err(ReadError::into_error)?;
    let row = selected_row(&rows)?;
    let selected = decode_target(row, target)?;
    if !original.same_object_state(&selected)
        || !selected.metadata.available()
        || ObservationBounds::new(&original.metadata.observed_at, &selected.metadata.observed_at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let memory = MssqlTableMemory {
        table: target.clone(),
        observed_at: selected.metadata.observed_at.clone(),
        allocated_table_kibibytes: optional_counter(row, "memory_allocated_for_table_kb")?,
        used_table_kibibytes: optional_counter(row, "memory_used_by_table_kb")?,
        allocated_indexes_kibibytes: optional_counter(row, "memory_allocated_for_indexes_kb")?,
        used_indexes_kibibytes: optional_counter(row, "memory_used_by_indexes_kb")?,
    };
    if !memory.is_valid() {
        return Err(invalid_data());
    }
    let present = optional_boolean(row, "row_present")?.ok_or_else(invalid_data)?;
    let point = if present {
        Some(memory)
    } else {
        if [
            &memory.allocated_table_kibibytes,
            &memory.used_table_kibibytes,
            &memory.allocated_indexes_kibibytes,
            &memory.used_indexes_kibibytes,
        ]
        .into_iter()
        .any(Option::is_some)
        {
            return Err(invalid_data());
        }
        None
    };
    Ok(Read { selected, point })
}
#[cfg(test)]
pub(crate) mod tests;
