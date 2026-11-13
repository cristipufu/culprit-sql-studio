//! One native field projection per source, shared by scans, admission and retained-history capture.
pub(super) struct HistoryTable {
    relation: &'static str,
    columns: fn() -> String,
    replica: Option<&'static str>,
}
pub(super) const DATABASE: HistoryTable = HistoryTable {
    relation: "sys.dm_db_resource_stats",
    columns: super::sample::columns,
    replica: Some("replica_role"),
};
pub(super) const POOL: HistoryTable = HistoryTable {
    relation: "sys.dm_elastic_pool_resource_stats",
    columns: super::pool::sample::columns,
    replica: None,
};
impl HistoryTable {
    pub(super) fn retained(&self) -> String {
        self.select("TOP (@P1) ", "", "ASC")
    }
    pub(super) fn latest(&self) -> String {
        self.select("TOP (1) CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at, ", "", "DESC")
    }
    pub(super) fn windowed(&self) -> String {
        self.select(
            "TOP (@P1) ",
            r#"WHERE end_time >= CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2)
 AND end_time < CAST(CONVERT(datetimeoffset,@P3,127) AS datetime2)
 AND (NULLIF(@P4,'') IS NULL OR end_time > CAST(CONVERT(datetimeoffset,NULLIF(@P4,''),127) AS datetime))"#,
            "ASC",
        )
    }
    fn select(&self, prefix: &str, filter: &str, order: &str) -> String {
        format!(
            "SELECT /* culprit */ {prefix}CONVERT(varchar(40),CAST(end_time AS datetime2),126)+'Z' AS sample_at,{} FROM {} {filter} ORDER BY end_time {order} OPTION(MAXDOP 1)",
            (self.columns)(),
            self.relation
        )
    }
    pub(super) fn inventory(&self) -> String {
        let replica = self.replica.map(|column| format!(", MAX({column}) AS replica_role")).unwrap_or_default();
        format!(
            r#"SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at,
 CONVERT(varchar(40),CAST(MIN(end_time) AS datetime2),126)+'Z' AS available_from,
 CONVERT(varchar(40),CAST(MAX(end_time) AS datetime2),126)+'Z' AS available_until,
 COUNT(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN 1 END) AS selected_count,
 CONVERT(varchar(40),CAST(MIN(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN end_time END) AS datetime2),126)+'Z' AS selected_first,
 CONVERT(varchar(40),CAST(MAX(CASE WHEN end_time >= CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
             AND end_time < CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2) THEN end_time END) AS datetime2),126)+'Z' AS selected_last,
 COUNT(*) AS retained_count{} FROM {} OPTION(MAXDOP 1)"#,
            replica, self.relation
        )
    }
}
