//! Table metadata and partition accounting share one native relation for scans and streamed capture.
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_text, text},
    value::Rec,
};
use culprit_core::{Result, TableStat, telemetry::*};

const MAX_APPLICATION_TABLES: usize = 10_000;
pub(crate) struct Layout {
    with_partition_statistics: bool,
}
impl Layout {
    pub(crate) fn new(with_partition_statistics: bool) -> Self {
        Self { with_partition_statistics }
    }
}

pub(crate) fn select_sql(layout: &Layout) -> String {
    let relation = crate::partition_storage::relation(layout.with_partition_statistics);
    let page_bytes = crate::partition_storage::PAGE_BYTES;
    let measurable = if layout.with_partition_statistics {
        "t.is_memory_optimized=0"
    } else {
        "t.is_memory_optimized=0 AND NOT EXISTS(SELECT 1 FROM sys.indexes ci WHERE ci.object_id=t.object_id AND ci.type IN (5,6))"
    };
    format!(
        r#"
WITH partitions AS ({relation}), totals AS (
 SELECT object_id,
  SUM(CASE WHEN index_id IN (0,1) THEN row_count ELSE 0 END) AS row_count,
  SUM(reserved_pages)*{page_bytes} AS reserved_bytes,
  SUM(CASE WHEN index_id IN (0,1) THEN reserved_pages ELSE 0 END)*{page_bytes} AS data_bytes,
  SUM(CASE WHEN index_id>1 THEN reserved_pages ELSE 0 END)*{page_bytes} AS index_bytes,
  SUM(CASE WHEN index_id IN (0,1) THEN in_row_pages ELSE 0 END)*{page_bytes} AS in_row_bytes,
  SUM(CASE WHEN index_id IN (0,1) THEN lob_pages ELSE 0 END)*{page_bytes} AS lob_bytes,
  SUM(CASE WHEN index_id IN (0,1) THEN overflow_pages ELSE 0 END)*{page_bytes} AS overflow_bytes
 FROM partitions GROUP BY object_id
)
SELECT TOP (@P1) DB_ID() AS database_id,t.object_id,s.name AS schema_name,t.name AS table_name,
 CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,
 CASE WHEN t.is_memory_optimized=0 THEN CONVERT(varchar(40),p.row_count) END AS row_count,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.reserved_bytes) END AS reserved_bytes,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.data_bytes) END AS data_bytes,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.index_bytes) END AS index_bytes,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.in_row_bytes) END AS in_row_bytes,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.lob_bytes) END AS lob_bytes,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),p.overflow_bytes) END AS overflow_bytes,
 CASE WHEN {measurable} THEN 1 ELSE 0 END AS sizes_available,
 CAST(OBJECTPROPERTY(t.object_id,'TableHasPrimaryKey') AS int) AS has_primary_key,
 CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION')=1
  THEN CASE WHEN EXISTS(SELECT 1 FROM sys.indexes i WHERE i.object_id=t.object_id AND i.index_id=0) THEN 1 ELSE 0 END END AS is_heap
FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id
LEFT JOIN totals p ON p.object_id=t.object_id
WHERE t.is_ms_shipped=0
ORDER BY t.object_id OPTION(MAXDOP 1)
"#
    )
}

pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<MssqlTableSnapshot> {
    let count = |name| optional_text(row, name);
    let sizes_available = optional_boolean(row, "sizes_available")?.ok_or_else(invalid_data)?;
    let size_source = if !sizes_available {
        MssqlTableSizeSource::Unavailable
    } else if layout.with_partition_statistics {
        MssqlTableSizeSource::PartitionStatistics
    } else {
        MssqlTableSizeSource::CatalogAllocations
    };
    let value = MssqlTableSnapshot {
        identity: MssqlObjectIdentity {
            database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
            object_id: integer(row, "object_id")?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        schema: text(row, "schema_name")?,
        name: text(row, "table_name")?,
        modified_without_offset: text(row, "modified_without_offset")?,
        estimated_rows: count("row_count")?,
        reserved_bytes: count("reserved_bytes")?,
        data_bytes: count("data_bytes")?,
        index_bytes: count("index_bytes")?,
        in_row_bytes: count("in_row_bytes")?,
        lob_bytes: count("lob_bytes")?,
        row_overflow_bytes: count("overflow_bytes")?,
        has_primary_key: optional_boolean(row, "has_primary_key")?,
        is_heap: optional_boolean(row, "is_heap")?,
        size_source,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

pub(crate) struct TableRead {
    pub tables: Vec<TableStat>,
    pub complete_scope: bool,
}
pub(crate) async fn application<R: NativeRead>(reader: &mut R, with_partition_statistics: bool) -> Result<TableRead> {
    let complete_scope = crate::catalog_visibility::complete(reader).await?;
    let layout = Layout::new(with_partition_statistics);
    let rows = reader
        .query(
            &select_sql(&layout),
            vec![Parameter::Integer((MAX_APPLICATION_TABLES + 1) as i64)],
            MAX_APPLICATION_TABLES + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_APPLICATION_TABLES {
        return Err(CollectionStop::RowLimit.error());
    }
    let tables = rows.iter().map(|row| decode(row, &layout).and_then(project)).collect::<Result<Vec<_>>>()?;
    Ok(TableRead { tables, complete_scope })
}
fn display(value: Option<String>) -> Option<f64> {
    value.and_then(|value| value.parse::<u64>().ok()).map(|value| value as f64)
}
fn project(value: MssqlTableSnapshot) -> Result<TableStat> {
    let has_primary_key = value.has_primary_key.ok_or_else(crate::catalog_visibility::unavailable)?;
    Ok(TableStat {
        schema: value.schema,
        name: value.name,
        kind: None,
        definition: None,
        rows: display(value.estimated_rows),
        total_bytes: display(value.reserved_bytes),
        data_bytes: display(value.data_bytes),
        index_bytes: display(value.index_bytes),
        in_row_bytes: display(value.in_row_bytes),
        lob_bytes: display(value.lob_bytes),
        overflow_bytes: display(value.row_overflow_bytes),
        has_primary_key,
        is_heap: value.is_heap,
        seq_scans: None,
        seq_rows_read: None,
        index_scans: None,
        inserts: None,
        updates: None,
        deletes: None,
        dead_rows: None,
        modifications_since_analyze: None,
        last_vacuum: None,
        last_analyze: None,
        forwarded_records: None,
        forwarded_fetches: None,
        xid_age: None,
        storage: None,
        partition_of: None,
        counters_since: None,
    })
}

#[cfg(test)]
pub(crate) mod tests;
