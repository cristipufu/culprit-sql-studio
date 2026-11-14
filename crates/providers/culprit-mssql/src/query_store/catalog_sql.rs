// Stable templates and bound parameters keep page values out of SQL text and the plan-cache key.
pub(crate) const EDITION: &str = "SELECT /* culprit */ CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition";
pub(crate) const OPTIONS: &str = concat!(
    "SELECT /* culprit */ *, ",
    super::variants_sql::readable!(),
    " AS variant_families FROM sys.database_query_store_options"
);
pub(crate) const IDENTITY: &str = r#"
SELECT /* culprit */ database_name, CONVERT(varchar(36), logical_database_guid) AS incarnation,
 CONVERT(varchar(36), physical_database_guid) AS physical_incarnation,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability
FROM sys.dm_user_db_resource_governance WHERE database_id = DB_ID() AND database_name = DB_NAME()
OPTION (MAXDOP 1)"#;
pub(crate) const REPLICAS: &str = "SELECT TOP (101) /* culprit */ replica_group_id FROM sys.query_store_replicas ORDER BY replica_group_id OPTION (MAXDOP 1)";
pub(crate) const MAX_PLAN: &str =
    "SELECT TOP (1) /* culprit */ plan_id FROM sys.query_store_plan ORDER BY plan_id DESC OPTION (MAXDOP 1)";
macro_rules! query_identity_columns {
    ($query_id:literal) => {
        $crate::query_store::catalog_sql::query_identity_columns!($query_id, "q", "")
    };
    ($query_id:literal, $query_alias:literal, $prefix:literal) => {
        concat!(
            $query_id,
            " AS ",
            $prefix,
            "query_id,",
            $query_alias,
            ".context_settings_id AS ",
            $prefix,
            "context_settings_id,",
            $query_alias,
            ".object_id AS ",
            $prefix,
            "object_id,",
            "CONVERT(varchar(16),",
            $query_alias,
            ".query_hash,2) AS ",
            $prefix,
            "query_hash,",
            "CONVERT(varchar(40),",
            $query_alias,
            ".initial_compile_start_time,127) AS ",
            $prefix,
            "query_initial_compile_at"
        )
    };
}
pub(crate) use query_identity_columns;

macro_rules! plan_identity_columns {
    ($plan_alias:literal, $prefix:literal) => {
        concat!(
            $plan_alias,
            ".plan_id AS ",
            $prefix,
            "definition_plan_id,",
            "CONVERT(varchar(16),",
            $plan_alias,
            ".query_plan_hash,2) AS ",
            $prefix,
            "plan_hash,",
            "CONVERT(varchar(40),",
            $plan_alias,
            ".initial_compile_start_time,127) AS ",
            $prefix,
            "plan_initial_compile_at"
        )
    };
}
pub(crate) use plan_identity_columns;

macro_rules! definition_columns {
    () => {
        concat!(
            query_identity_columns!("q.query_id"),
            ",",
            plan_identity_columns!("p", ""),
            ",OBJECT_SCHEMA_NAME(q.object_id)+'.'+OBJECT_NAME(q.object_id) AS object_name\n"
        )
    };
}

pub(crate) fn runtime(range: bool) -> String {
    let filter = if range { " AND r.replica_group_id=@P3 AND r.plan_id>=@P4 AND r.plan_id<@P5" } else { "" };
    format!(
        "{}{}{}{}",
        super::runtime::relation(false),
        concat!(
            r#"
SELECT TOP (@P1) /* culprit */ r.runtime_stats_id AS row_id,r.plan_id,r.replica_group_id,r.execution_type,
 count_executions,avg_duration,stdev_duration,min_duration,max_duration,avg_cpu_time,avg_logical_io_reads,avg_physical_io_reads,
 avg_logical_io_writes,avg_rowcount,avg_dop,avg_query_max_used_memory,avg_clr_time,avg_log_bytes_used,avg_tempdb_space_used,
 CONVERT(varchar(40),first_execution_time,127) AS first_execution_at,
 CONVERT(varchar(40),r.last_execution_time,127) AS last_execution_at,
"#,
            definition_columns!(),
            r#"FROM culprit_runtime r
LEFT JOIN sys.query_store_plan p ON p.plan_id=r.plan_id
LEFT JOIN sys.query_store_query q ON q.query_id=p.query_id
WHERE r.runtime_stats_interval_id=@P2"#
        ),
        filter,
        " ORDER BY r.plan_id,r.execution_type,r.replica_group_id,r.runtime_stats_id OPTION(MAXDOP 1)"
    )
}
pub(crate) fn waits(range: bool) -> String {
    let filter = if range { " AND r.replica_group_id=@P3 AND r.plan_id>=@P4 AND r.plan_id<@P5" } else { "" };
    format!(
        "{}{}{}{}",
        super::waits::RELATION,
        concat!(
            r#"
SELECT TOP (@P1) /* culprit */ r.wait_stats_id AS row_id,r.plan_id,r.replica_group_id,r.execution_type,
 wait_category,total_query_wait_time_ms,avg_query_wait_time_ms,min_query_wait_time_ms,max_query_wait_time_ms,
"#,
            definition_columns!(),
            r#"FROM culprit_waits r
LEFT JOIN sys.query_store_plan p ON p.plan_id=r.plan_id
LEFT JOIN sys.query_store_query q ON q.query_id=p.query_id
WHERE r.runtime_stats_interval_id=@P2"#
        ),
        filter,
        " ORDER BY r.plan_id,r.execution_type,r.replica_group_id,r.wait_category,r.wait_stats_id OPTION(MAXDOP 1)"
    )
}

// Read lengths first. Payload queries return complete bounded values or NULL, never truncated definitions.
pub(crate) const QUERY_LENGTHS: &str = r#"
DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ ids.id, DATALENGTH(t.query_sql_text) AS source_bytes
FROM @ids.nodes('/ids/id') n(x)
CROSS APPLY(SELECT x.value('@id[1]','bigint') AS id) ids
LEFT JOIN sys.query_store_query q ON q.query_id = ids.id
LEFT JOIN sys.query_store_query_text t ON t.query_text_id = q.query_text_id
ORDER BY ids.id OPTION (MAXDOP 1)"#;
pub(crate) const PLAN_LENGTHS: &str = r#"
DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ ids.id, DATALENGTH(p.query_plan) AS source_bytes
FROM @ids.nodes('/ids/id') n(x)
CROSS APPLY(SELECT x.value('@id[1]','bigint') AS id) ids
LEFT JOIN sys.query_store_plan p ON p.plan_id = ids.id
ORDER BY ids.id OPTION (MAXDOP 1)"#;

const QUERY_DEFINITION_PREFIX: &str = concat!(
    "DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);\nSELECT /* culprit */ ids.id,",
    query_identity_columns!("q.query_id"),
);
const QUERY_DEFINITION_COLUMNS: &str = r#",
 CAST(q.is_internal_query AS int) AS engine_internal, CAST(q.is_clouddb_internal_query AS int) AS cloud_internal, OBJECT_SCHEMA_NAME(q.object_id)+'.'+OBJECT_NAME(q.object_id) AS object_name,
 DATALENGTH(t.query_sql_text) AS source_bytes,
 t.is_part_of_encrypted_module AS encrypted, t.has_restricted_text AS restricted,
 CASE WHEN DATALENGTH(t.query_sql_text) <= @P2 AND t.is_part_of_encrypted_module = 0
       AND t.has_restricted_text = 0 THEN t.query_sql_text END AS payload,
 c.context_settings_id AS observed_context_settings_id,
 CONVERT(varchar(16), c.set_options, 2) AS set_options, c.language_id, c.date_format, c.date_first,
 CONVERT(varchar(4), c.status, 2) AS status, c.required_cursor_options, c.acceptable_cursor_options,
 c.merge_action_type, c.default_schema_id"#;
const QUERY_DEFINITION_JOINS: &str = r#"
FROM @ids.nodes('/ids/id') n(x)
CROSS APPLY(SELECT x.value('@id[1]','bigint') AS id) ids
LEFT JOIN sys.query_store_query q ON q.query_id = ids.id
LEFT JOIN sys.query_store_query_text t ON t.query_text_id = q.query_text_id
LEFT JOIN sys.query_context_settings c ON c.context_settings_id = q.context_settings_id"#;
const DEFINITION_ORDER: &str = "\nORDER BY ids.id OPTION (MAXDOP 1)";

pub(crate) fn query_definitions(with_families: bool) -> String {
    let (columns, joins) =
        if with_families { (super::variants_sql::COLUMNS, super::variants_sql::JOINS) } else { ("", "") };
    format!(
        "{QUERY_DEFINITION_PREFIX}{QUERY_DEFINITION_COLUMNS}{columns}{QUERY_DEFINITION_JOINS}{joins}{DEFINITION_ORDER}"
    )
}

pub(crate) const PLAN_DEFINITIONS: &str = concat!(
    r#"
DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ ids.id,
"#,
    definition_columns!(),
    r#",
 p.engine_version, p.compatibility_level, DATALENGTH(p.query_plan) AS source_bytes,
 t.is_part_of_encrypted_module AS encrypted, t.has_restricted_text AS restricted,
 CASE WHEN DATALENGTH(p.query_plan) <= @P2 AND t.is_part_of_encrypted_module = 0
       AND t.has_restricted_text = 0 THEN p.query_plan END AS payload
FROM @ids.nodes('/ids/id') n(x)
CROSS APPLY(SELECT x.value('@id[1]','bigint') AS id) ids
LEFT JOIN sys.query_store_plan p ON p.plan_id = ids.id
LEFT JOIN sys.query_store_query q ON q.query_id = p.query_id
LEFT JOIN sys.query_store_query_text t ON t.query_text_id = q.query_text_id
ORDER BY ids.id OPTION (MAXDOP 1)"#
);

/// Query Store's queries behind the database's missing-index requests: the optimizer's request groups name the
/// query_hash of each query that asked (SQL Server 2019+, Azure SQL). DMVs and Query Store's catalog only.
pub(crate) const MISSING_INDEX_ASKERS_SQL: &str = r#"
SELECT DISTINCT TOP (200) qsq.query_id
FROM sys.dm_db_missing_index_group_stats_query m
JOIN sys.dm_db_missing_index_groups g ON g.index_group_handle = m.group_handle
JOIN sys.dm_db_missing_index_details d ON d.index_handle = g.index_handle
JOIN sys.query_store_query qsq ON qsq.query_hash = m.query_hash
WHERE d.database_id = DB_ID()
"#;
