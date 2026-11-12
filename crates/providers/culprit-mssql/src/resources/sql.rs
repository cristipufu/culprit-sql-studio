//! One native field projection; callers request retained, windowed or latest evidence.
macro_rules! samples {
    ($prefix:literal, $filter:literal, $order:literal) => {
        concat!(
            "SELECT /* culprit */ ",
            $prefix,
            r#"CONVERT(varchar(40),CAST(end_time AS datetime2),126) + 'Z' AS sample_at,
 replica_role, avg_cpu_percent, avg_data_io_percent, avg_log_write_percent, avg_memory_usage_percent,
 max_worker_percent, max_session_percent, xtp_storage_percent,
 avg_instance_cpu_percent, avg_instance_memory_percent, dtu_limit, cpu_limit
FROM sys.dm_db_resource_stats "#,
            $filter,
            " ORDER BY end_time ",
            $order,
            " OPTION(MAXDOP 1)"
        )
    };
}
pub(super) const RETAINED: &str = samples!("TOP (@P1) ", "", "ASC");
pub(super) const WINDOWED: &str = samples!(
    "TOP (@P1) ",
    r#"
WHERE end_time >= CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2)
 AND end_time < CAST(CONVERT(datetimeoffset,@P3,127) AS datetime2)
 AND (NULLIF(@P4,'') IS NULL OR end_time > CAST(CONVERT(datetimeoffset,NULLIF(@P4,''),127) AS datetime))
"#,
    "ASC"
);
pub(super) const LATEST: &str =
    samples!("TOP (1) CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at, ", "", "DESC");
pub(super) const INVENTORY: &str = r#"
SELECT /* culprit */ CONVERT(varchar(40), SYSUTCDATETIME(), 126) + 'Z' AS observed_at,
 CONVERT(varchar(40), CAST(MIN(end_time) AS datetime2), 126) + 'Z' AS available_from,
 CONVERT(varchar(40), CAST(MAX(end_time) AS datetime2), 126) + 'Z' AS available_until,
 COUNT(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN 1 END) AS selected_count,
 CONVERT(varchar(40), CAST(MIN(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN end_time END) AS datetime2),126) + 'Z' AS selected_first,
 CONVERT(varchar(40), CAST(MAX(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN end_time END) AS datetime2),126) + 'Z' AS selected_last,
 COUNT(*) AS retained_count, MAX(replica_role) AS replica_role
FROM sys.dm_db_resource_stats OPTION (MAXDOP 1)"#;
