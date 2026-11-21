//! Catalog indexes retain native column identities and distinguish absent usage rows from unsupported counters.
use crate::{
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_boolean, optional_counter,
        optional_text, text,
    },
    value::Rec,
};
use culprit_core::{IndexStat, Result, telemetry::*};

mod columnstore;
pub(crate) mod configuration;
const APPLICATION_INDEXES: usize = MAX_CURRENT_MSSQL_INDEX_ROWS;
pub(crate) struct Layout {
    pub with_dmvs: bool,
    pub expression_limit: usize,
}
pub(crate) fn select_sql(layout: &Layout) -> String {
    let relation = crate::partition_storage::relation(layout.with_dmvs);
    let page_bytes = crate::partition_storage::PAGE_BYTES;
    let measurable = if layout.with_dmvs {
        "(o.type='V' OR native_table.is_memory_optimized=0)"
    } else {
        "(o.type='V' OR native_table.is_memory_optimized=0) AND i.type NOT IN (5,6)"
    };
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
    columnstore::query(columnstore::QueryParts {
        before_configuration: format!(
            r#"
WITH native_partitions AS ({relation})
SELECT TOP (@P1) DB_ID() AS database_id,o.object_id,
 CONVERT(varchar(40),o.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),o.modify_date,126) AS modified_without_offset,
 s.name AS schema_name,o.name AS parent_name,i.name AS index_name,i.index_id,i.type,i.type_desc,
 RTRIM(o.type) AS parent_native_type,native_table.is_memory_optimized AS parent_memory_optimized,i.is_unique,i.is_primary_key,i.is_disabled,i.has_filter,{filter},"#
        ),
        before_ordinals: format!(
            " ISNULL((SELECT TOP({}) ic.index_column_id,c.column_id,c.name,ic.key_ordinal,
         ic.is_descending_key AS descending,ic.is_included_column AS included,",
            MAX_MSSQL_INDEX_COLUMNS + 1
        ),
        after_ordinals: format!(
            r#"
  FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
  WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id ORDER BY ic.index_column_id FOR JSON PATH,INCLUDE_NULL_VALUES),N'[]') AS index_columns,
 CASE WHEN {measurable} THEN CONVERT(varchar(40),(SELECT SUM(p.reserved_pages)*{page_bytes} FROM native_partitions p WHERE p.object_id=i.object_id AND p.index_id=i.index_id)) END AS reserved_bytes,
 {usage_fields}
FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
LEFT JOIN sys.tables native_table ON native_table.object_id=o.object_id AND o.type='U'
JOIN sys.schemas s ON s.schema_id=o.schema_id {usage_join}
WHERE o.is_ms_shipped=0 AND o.type IN('U','V') AND i.type>0 AND i.is_hypothetical=0
ORDER BY i.object_id,i.index_id OPTION(MAXDOP 1)
"#
        ),
    })
}
pub(crate) struct NativeIndex {
    pub snapshot: MssqlIndexSnapshot,
    raw_filter: Option<String>,
    display_last_read: Option<String>,
}
pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<NativeIndex> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let type_code = narrow("type")?;
    let parent_kind = match text(row, "parent_native_type")?.as_str() {
        "U" => MssqlIndexParentKind::Table,
        "V" => MssqlIndexParentKind::IndexedView,
        _ => return Err(invalid_data()),
    };
    let parent_memory_optimized = optional_boolean(row, "parent_memory_optimized")?;
    // The DMV excludes spatial and memory-optimized indexes. Their missing rows cannot establish zero use.
    // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-index-usage-stats-transact-sql
    let usage = if !layout.with_dmvs {
        MssqlIndexUsage::NotCollected
    } else if parent_memory_optimized == Some(true) || type_code == 4 {
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
    let columns = columnstore::columns(&text(row, "index_columns")?)?;
    let mut snapshot = MssqlIndexSnapshot {
        parent_kind,
        parent_object: MssqlObjectIdentity {
            database_id: narrow("database_id")?,
            object_id: narrow("object_id")?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        parent_schema: text(row, "schema_name")?,
        parent_name: text(row, "parent_name")?,
        index_id: narrow("index_id")?,
        name: text(row, "index_name")?,
        parent_modified_without_offset: text(row, "modified_without_offset")?,
        type_code,
        native_type: text(row, "type_desc")?,
        parent_memory_optimized,
        unique: boolean(row, "is_unique")?,
        primary_key: boolean(row, "is_primary_key")?,
        disabled: boolean(row, "is_disabled")?,
        columns: columns.native,
        columnstore: None,
        configuration: None,
        filter: crate::catalog_expression::decode(row, boolean(row, "has_filter")?, "filter", layout.expression_limit)?,
        reserved_bytes: optional_text(row, "reserved_bytes")?,
        usage,
    };
    snapshot.columnstore = columnstore::configuration(row, &snapshot, columns.columnstore)?;
    snapshot.configuration = Some(configuration::index(row, &snapshot)?);
    if !snapshot.is_valid() {
        return Err(invalid_data());
    }
    Ok(NativeIndex {
        snapshot,
        raw_filter: optional_text(row, "filter_value")?,
        display_last_read: optional_text(row, "display_last_read")?,
    })
}
pub(crate) struct IndexRead {
    pub table_indexes: Vec<IndexStat>,
    pub indexed_views: Vec<MssqlIndexSnapshot>,
    pub retained_index_rows: usize,
    pub parent_prefix_complete: bool,
    pub with_usage: bool,
}
pub(crate) async fn application<R: NativeRead>(reader: &mut R, with_dmvs: bool) -> Result<IndexRead> {
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
    if rows.len() > APPLICATION_INDEXES + 1 {
        return Err(CollectionStop::RowLimit.error());
    }
    let parent_prefix_complete = rows.len() <= APPLICATION_INDEXES;
    // Even the unretained lookahead must satisfy the original kind, identity, value and scope contract.
    let mut native = rows.iter().map(|row| decode(row, &layout)).collect::<Result<Vec<_>>>()?;
    if !mssql_index_rows_are_consistent(native.iter().map(|index| &index.snapshot)) {
        return Err(CollectionStop::SourceChanged.error());
    }
    native.truncate(APPLICATION_INDEXES);
    let retained_index_rows = native.len();
    let mut table_indexes = Vec::new();
    let mut indexed_views = Vec::new();
    for index in native {
        match index.snapshot.parent_kind {
            MssqlIndexParentKind::Table => table_indexes.push(index.project()?),
            MssqlIndexParentKind::IndexedView => indexed_views.push(index.snapshot),
        }
    }
    Ok(IndexRead {
        table_indexes,
        indexed_views,
        retained_index_rows,
        parent_prefix_complete,
        with_usage: layout.with_dmvs,
    })
}

impl NativeIndex {
    fn project(self) -> Result<IndexStat> {
        let index = self.snapshot;
        if index.parent_kind != MssqlIndexParentKind::Table {
            return Err(invalid_data());
        }
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
            mssql_columnstore: index.columnstore,
            mssql_configuration: index.configuration,
            docdb: None,
            filter_facts: None,
            schema: index.parent_schema,
            table: index.parent_name,
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

#[cfg(test)]
mod native_tests;
