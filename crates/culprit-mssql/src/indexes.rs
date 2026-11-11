//! Catalog indexes retain native column identities and distinguish absent usage rows from unsupported counters.
use crate::{
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_boolean, optional_counter,
        optional_text, text,
    },
    value::Rec,
};
use culprit_core::{IndexStat, Result, telemetry::*};

const APPLICATION_INDEXES: usize = 10_000;
pub(crate) struct Layout {
    pub with_dmvs: bool,
    pub expression_limit: usize,
}
pub(crate) fn select_sql(layout: &Layout) -> String {
    let relation = crate::partition_storage::relation(layout.with_dmvs);
    let page_bytes = crate::partition_storage::PAGE_BYTES;
    let measurable =
        if layout.with_dmvs { "t.is_memory_optimized=0" } else { "t.is_memory_optimized=0 AND i.type NOT IN (5,6)" };
    let filter = crate::catalog_expression::select("i.filter_definition", "filter", layout.expression_limit);
    let usage_fields = if layout.with_dmvs {
        let display = crate::sql_time::utc_local_iso(
            "(SELECT MAX(v) FROM (VALUES (us.last_user_seek),(us.last_user_scan),(us.last_user_lookup)) AS x(v))",
        );
        format!(
            "CASE WHEN us.object_id IS NULL THEN 0 ELSE 1 END AS usage_row_present,us.user_seeks,us.user_scans,us.user_lookups,us.user_updates,CONVERT(varchar(40),us.last_user_seek,126) AS last_user_seek,CONVERT(varchar(40),us.last_user_scan,126) AS last_user_scan,CONVERT(varchar(40),us.last_user_lookup,126) AS last_user_lookup,CONVERT(varchar(40),us.last_user_update,126) AS last_user_update,{display} AS display_last_read"
        )
    } else {
        "CAST(NULL AS int) AS usage_row_present,CAST(NULL AS bigint) AS user_seeks,CAST(NULL AS bigint) AS user_scans,CAST(NULL AS bigint) AS user_lookups,CAST(NULL AS bigint) AS user_updates,CAST(NULL AS varchar(40)) AS last_user_seek,CAST(NULL AS varchar(40)) AS last_user_scan,CAST(NULL AS varchar(40)) AS last_user_lookup,CAST(NULL AS varchar(40)) AS last_user_update,CAST(NULL AS varchar(40)) AS display_last_read".into()
    };
    let usage_join = if layout.with_dmvs {
        "LEFT JOIN sys.dm_db_index_usage_stats us ON us.database_id=DB_ID() AND us.object_id=i.object_id AND us.index_id=i.index_id"
    } else {
        ""
    };
    format!(
        r#"
WITH native_partitions AS ({relation})
SELECT TOP (@P1) DB_ID() AS database_id,t.object_id,
 CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,
 s.name AS schema_name,t.name AS table_name,i.name AS index_name,i.index_id,i.type,i.type_desc,
 t.is_memory_optimized,i.is_unique,i.is_primary_key,i.is_disabled,i.has_filter,{filter},
 (SELECT ic.index_column_id,c.column_id,c.name,ic.key_ordinal,
         ic.is_descending_key AS descending,ic.is_included_column AS included
  FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
  WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id ORDER BY ic.index_column_id FOR JSON PATH) AS index_columns,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),(SELECT SUM(p.reserved_pages)*{page_bytes} FROM native_partitions p WHERE p.object_id=i.object_id AND p.index_id=i.index_id)) END AS reserved_bytes,
 {usage_fields}
FROM sys.indexes i JOIN sys.tables t ON t.object_id=i.object_id
JOIN sys.schemas s ON s.schema_id=t.schema_id {usage_join}
WHERE t.is_ms_shipped=0 AND i.type>0 AND i.is_hypothetical=0
ORDER BY i.object_id,i.index_id OPTION(MAXDOP 1)
"#
    )
}
pub(crate) struct NativeIndex {
    pub snapshot: MssqlIndexSnapshot,
    raw_filter: Option<String>,
    display_last_read: Option<String>,
}
pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<NativeIndex> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let type_code = narrow("type")?;
    let memory_optimized = boolean(row, "is_memory_optimized")?;
    // The DMV excludes spatial and memory-optimized indexes. Their missing rows cannot establish zero use.
    // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-index-usage-stats-transact-sql
    let usage = if !layout.with_dmvs {
        MssqlIndexUsage::NotCollected
    } else if memory_optimized || type_code == 4 {
        MssqlIndexUsage::NotApplicable
    } else {
        MssqlIndexUsage::Observed(Box::new(MssqlIndexUsageCounters {
            row_present: optional_boolean(row, "usage_row_present")?.ok_or_else(invalid_data)?,
            user_seeks: optional_counter(row, "user_seeks")?,
            user_scans: optional_counter(row, "user_scans")?,
            user_lookups: optional_counter(row, "user_lookups")?,
            user_updates: optional_counter(row, "user_updates")?,
            last_user_seek_without_offset: optional_text(row, "last_user_seek")?,
            last_user_scan_without_offset: optional_text(row, "last_user_scan")?,
            last_user_lookup_without_offset: optional_text(row, "last_user_lookup")?,
            last_user_update_without_offset: optional_text(row, "last_user_update")?,
            reset: culprit_core::ResetMarker::Unknown,
        }))
    };
    let snapshot = MssqlIndexSnapshot {
        table: MssqlObjectIdentity {
            database_id: narrow("database_id")?,
            object_id: narrow("object_id")?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        schema: text(row, "schema_name")?,
        table_name: text(row, "table_name")?,
        index_id: narrow("index_id")?,
        name: text(row, "index_name")?,
        table_modified_without_offset: text(row, "modified_without_offset")?,
        type_code,
        native_type: text(row, "type_desc")?,
        memory_optimized,
        unique: boolean(row, "is_unique")?,
        primary_key: boolean(row, "is_primary_key")?,
        disabled: boolean(row, "is_disabled")?,
        columns: serde_json::from_str(&text(row, "index_columns")?).map_err(|_| invalid_data())?,
        filter: crate::catalog_expression::decode(row, boolean(row, "has_filter")?, "filter", layout.expression_limit)?,
        reserved_bytes: optional_text(row, "reserved_bytes")?,
        usage,
    };
    if !snapshot.is_valid() {
        return Err(invalid_data());
    }
    Ok(NativeIndex {
        snapshot,
        raw_filter: optional_text(row, "filter_value")?,
        display_last_read: optional_text(row, "display_last_read")?,
    })
}
pub(crate) async fn application<R: NativeRead>(reader: &mut R, with_dmvs: bool) -> Result<(Vec<IndexStat>, bool)> {
    let mut layout = Layout { with_dmvs, expression_limit: MAX_CATALOG_EXPRESSION_BYTES };
    let read = |layout: &Layout| (select_sql(layout), vec![Parameter::Integer((APPLICATION_INDEXES + 1) as i64)]);
    let (sql, parameters) = read(&layout);
    let rows = match reader.query(&sql, parameters, APPLICATION_INDEXES + 1).await {
        Ok(rows) => rows,
        Err(ReadError::Source(error)) if with_dmvs && error.code == culprit_core::ErrorCode::PermissionDenied => {
            layout.with_dmvs = false;
            let (sql, parameters) = read(&layout);
            reader.query(&sql, parameters, APPLICATION_INDEXES + 1).await.map_err(ReadError::into_error)?
        }
        Err(error) => return Err(error.into_error()),
    };
    if rows.len() > APPLICATION_INDEXES {
        return Err(CollectionStop::RowLimit.error());
    }
    let indexes =
        rows.iter().map(|row| decode(row, &layout).and_then(NativeIndex::project)).collect::<Result<Vec<_>>>()?;
    Ok((indexes, layout.with_dmvs))
}
impl NativeIndex {
    fn project(self) -> Result<IndexStat> {
        let index = self.snapshot;
        if let Some(filter) = &index.filter {
            crate::catalog_expression::require_available(filter)?;
        }
        let counter = |field: fn(&MssqlIndexUsageCounters) -> &Option<String>| match &index.usage {
            MssqlIndexUsage::Observed(usage) if !usage.row_present => Some(0.0),
            MssqlIndexUsage::Observed(usage) => {
                field(usage).as_ref().and_then(|value| value.parse::<u64>().ok()).map(|value| value as f64)
            }
            _ => None,
        };
        let (seeks, scans, lookups, writes) = (
            counter(|v| &v.user_seeks),
            counter(|v| &v.user_scans),
            counter(|v| &v.user_lookups),
            counter(|v| &v.user_updates),
        );
        let mut keys = index.columns.iter().filter(|column| column.key_ordinal > 0).collect::<Vec<_>>();
        keys.sort_by_key(|column| column.key_ordinal);
        let columns = keys
            .into_iter()
            .map(|column| if column.descending { format!("{} DESC", column.name) } else { column.name.clone() })
            .collect();
        Ok(IndexStat {
            docdb: None,
            filter_facts: None,
            schema: index.schema,
            table: index.table_name,
            name: index.name,
            kind: index.native_type.to_ascii_lowercase(),
            columns,
            include: index.columns.into_iter().filter(|column| column.included).map(|column| column.name).collect(),
            filter: self.raw_filter,
            is_unique: index.unique,
            is_primary_key: index.primary_key,
            is_clustered: matches!(index.type_code, 1 | 5),
            is_disabled: index.disabled,
            state: None,
            maintained: None,
            has_statistics: None,
            size_bytes: index.reserved_bytes.and_then(|value| value.parse::<u64>().ok()).map(|value| value as f64),
            reads: seeks.zip(scans).zip(lookups).map(|((a, b), c)| a + b + c),
            writes,
            seeks,
            scans,
            lookups,
            last_read: self.display_last_read,
            definition: None,
            contention: None,
            created_at_latest: None,
            partition_of: None,
            opclasses: None,
            counters_since: None,
        })
    }
}
#[cfg(test)]
pub(crate) mod tests;
