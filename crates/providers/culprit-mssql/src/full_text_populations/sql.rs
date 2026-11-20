//! Published metadata only. TOP caps the response, not the DMV's internal work; this source is Unqualified.
const TARGET: &str = "DB_ID() AS database_id,t.object_id,s.name AS schema_name,t.name AS table_name,CONVERT(varchar(40),t.create_date,126) AS created_without_offset,CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,ft.fulltext_catalog_id AS catalog_id,ft.unique_index_id,HAS_PERMS_BY_NAME(QUOTENAME(s.name)+N'.'+QUOTENAME(t.name),N'OBJECT',N'VIEW DEFINITION') AS can_view_definition";
const RELATION: &str = "FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id JOIN sys.fulltext_indexes ft ON ft.object_id=t.object_id";
const SELECTED: &str =
    "t.object_id=@P2 AND ft.fulltext_catalog_id=@P3 AND ft.unique_index_id=@P4 AND t.is_ms_shipped=0";

pub(super) fn target() -> String {
    format!(
        "SELECT TOP(2) /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,{TARGET} {RELATION} WHERE {SELECTED} OPTION(MAXDOP 1)"
    )
}
pub(super) fn populations(permission: super::StatePermission) -> String {
    let (permission_check, native_authorization) = match permission {
        super::StatePermission::ServerState => ("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER STATE')", 0),
        super::StatePermission::ServerPerformanceState => {
            ("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER PERFORMANCE STATE')", 0)
        }
        // Azure Basic/S0/S1/pools need different native rights. The actual DMV authorizes this one
        // query; no guessed tier, role-name check, timeout/denial retry or VIEW DATABASE STATE proof.
        super::StatePermission::AzureDatabase => ("NULL", 1),
    };
    let active = format!(
        "SELECT TOP(@P1) /* culprit */ CONVERT(varchar(40),@at,127)+'Z' AS observed_at,{TARGET},\
         CAST(1 AS int) AS can_read_state,CAST(1 AS int) AS layout_available,\
         CASE WHEN p.database_id IS NULL THEN 0 ELSE 1 END AS population_present,\
         p.database_id AS population_database_id,p.table_id AS population_table_id,p.catalog_id AS population_catalog_id,\
         p.population_type,p.population_type_description,p.status,p.status_description,\
         p.completion_type,p.completion_type_description,p.queued_population_type,p.queued_population_type_description,\
         p.is_clustered_index_scan,p.range_count,p.completed_range_count,p.outstanding_batch_count,\
         CONVERT(varchar(40),CAST(p.start_time AS datetime2),126) AS started_without_offset \
         {RELATION} LEFT JOIN sys.dm_fts_index_population p ON p.database_id=DB_ID() AND p.table_id=t.object_id AND p.catalog_id=ft.fulltext_catalog_id \
         WHERE {SELECTED} OPTION(MAXDOP 1)"
    );
    let active = crate::catalog_visibility::sql_literal(&active);
    let unavailable = format!(
        "SELECT TOP(1) /* culprit */ CONVERT(varchar(40),@at,127)+'Z' AS observed_at,{TARGET},\
         @permission AS can_read_state,@layout AS layout_available,CAST(0 AS int) AS population_present \
         {RELATION} WHERE {SELECTED} OPTION(MAXDOP 1)"
    );
    // Description widths vary across native builds. Validate the nvarchar type; decoding bounds each value.
    format!(
        "DECLARE /* culprit */ @at datetime2(7)=SYSUTCDATETIME(),@permission int={permission_check};\
         DECLARE /* culprit */ @layout int=CASE WHEN (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_fts_index_population') AND \
         ((name IN(N'database_id',N'table_id',N'catalog_id',N'population_type',N'status',N'completion_type',N'queued_population_type',N'range_count',N'completed_range_count',N'outstanding_batch_count') AND system_type_id=56)\
         OR(name IN(N'population_type_description',N'status_description',N'completion_type_description',N'queued_population_type_description') AND system_type_id=231)\
         OR(name=N'is_clustered_index_scan' AND system_type_id=104) OR(name=N'start_time' AND system_type_id=61)))=16 THEN 1 ELSE 0 END;\
         IF (@permission=1 OR {native_authorization}=1) AND @layout=1 EXEC /* culprit */ sys.sp_executesql {active},N'@P1 int,@P2 int,@P3 int,@P4 int,@at datetime2(7)',@P1=@P1,@P2=@P2,@P3=@P3,@P4=@P4,@at=@at;\
         ELSE BEGIN {unavailable}; END;"
    )
}
