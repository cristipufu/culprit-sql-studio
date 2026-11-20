//! Exact selected catalog predicates cap returned rows, not internal server work or a complete index footprint.
pub(super) const TARGET: &str = r#"SELECT TOP(2) /* culprit */
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,DB_ID() AS database_id,
 t.object_id,i.index_id,CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,i.type AS index_type,t.is_memory_optimized,
 HAS_PERMS_BY_NAME(QUOTENAME(s.name)+N'.'+QUOTENAME(t.name),N'OBJECT',N'VIEW DEFINITION') AS can_view_definition,
 CASE WHEN @P3=0 OR EXISTS(SELECT 1 FROM sys.partitions p WHERE p.object_id=t.object_id AND p.index_id=i.index_id AND p.partition_number=@P3) THEN 1 ELSE 0 END AS partition_present,
 CASE WHEN (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.column_store_row_groups') AND
 ((name IN(N'object_id',N'index_id',N'partition_number',N'row_group_id') AND system_type_id=56)
 OR(name IN(N'delta_store_hobt_id',N'total_rows',N'deleted_rows',N'size_in_bytes') AND system_type_id=127)
 OR(name=N'state' AND system_type_id=48)))=9 THEN 1 ELSE 0 END AS layout_available
 FROM sys.tables t JOIN sys.indexes i ON i.object_id=t.object_id JOIN sys.schemas s ON s.schema_id=t.schema_id
 WHERE t.object_id=@P1 AND i.index_id=@P2 AND t.is_ms_shipped=0 OPTION(MAXDOP 1)"#;

pub(super) const ROWGROUPS: &str = r#"DECLARE /* culprit */ @culprit_rowgroups_at datetime2(7)=SYSUTCDATETIME();
 SELECT TOP(@P1) /* culprit */ DB_ID() AS database_id,
 CONVERT(varchar(40),@culprit_rowgroups_at,127)+'Z' AS observed_at,
 rg.object_id,rg.index_id,rg.partition_number,rg.row_group_id,i.type AS index_type,
 rg.delta_store_hobt_id,rg.state,rg.total_rows,rg.deleted_rows,rg.size_in_bytes
 FROM sys.column_store_row_groups rg JOIN sys.indexes i ON i.object_id=rg.object_id AND i.index_id=rg.index_id
 WHERE rg.object_id=@P2 AND rg.index_id=@P3 AND(@P4=0 OR rg.partition_number=@P4) OPTION(MAXDOP 1)"#;
