//! Only supplied handles invoke the native attribute function. No cache discovery, ranking or plan XML read.
use crate::cached_statements;

pub(super) fn selected() -> String {
    let context = cached_statements::context_columns_from("qs", "st", "db.native_value");
    // creation_time is native datetime. Widening its serialized milliseconds to datetime2 can compare unequal
    // to an unchanged native value (.333 vs .3333333 at compatibility 130+).
    // https://learn.microsoft.com/sql/t-sql/data-types/datetime2-transact-sql#convert-other-date-and-time-types-to-the-datetime2-data-type
    format!(
        r#"DECLARE /* culprit */ @selection xml=CONVERT(xml,@P1),@at datetime2=SYSUTCDATETIME();
DECLARE @statements TABLE(plan_handle varbinary(64),sql_handle varbinary(64),start_offset int,end_offset int,
 created datetime,generation bigint,database_id int);
INSERT @statements SELECT CONVERT(varbinary(64),n.x.value('@plan[1]','varchar(130)'),1),
 CONVERT(varbinary(64),n.x.value('@sql[1]','varchar(130)'),1),n.x.value('@start[1]','int'),n.x.value('@end[1]','int'),
 CONVERT(datetime,n.x.value('@created[1]','varchar(64)'),126),n.x.value('@generation[1]','bigint'),n.x.value('@database[1]','int')
FROM @selection.nodes('/statements/statement') n(x);
DECLARE @handles TABLE(plan_handle varbinary(64) PRIMARY KEY);
INSERT @handles SELECT DISTINCT plan_handle FROM @statements;
DECLARE @attributes TABLE(plan_handle varbinary(64),name varchar(128),native_type varchar(128),
 native_value sql_variant,is_cache_key bit);
INSERT @attributes
SELECT selected.plan_handle,a.attribute,CONVERT(varchar(128),SQL_VARIANT_PROPERTY(a.value,'BaseType')),a.value,a.is_cache_key
FROM @handles selected CROSS APPLY(SELECT TOP (@P2) attribute,value,is_cache_key
 FROM sys.dm_exec_plan_attributes(selected.plan_handle)
 WHERE is_cache_key=1 OR attribute IN('dbid','sql_handle')) a OPTION(MAXDOP 1,LOOP JOIN);
SELECT /* culprit */ 'clock' AS row_kind,CONVERT(varchar(40),@at,126)+'Z' AS observed_at;
SELECT /* culprit */ 'statement' AS row_kind,CONVERT(varchar(40),@at,126)+'Z' AS observed_at,{context}
FROM @statements selected
JOIN sys.dm_exec_query_stats qs ON qs.plan_handle=selected.plan_handle AND qs.sql_handle=selected.sql_handle
 AND qs.statement_start_offset=selected.start_offset AND qs.statement_end_offset=selected.end_offset
 AND qs.creation_time=selected.created AND qs.plan_generation_num=selected.generation
JOIN @attributes db ON db.plan_handle=selected.plan_handle AND db.name='dbid'
OUTER APPLY sys.dm_exec_sql_text(qs.sql_handle) st
WHERE CONVERT(int,db.native_value)=DB_ID() AND selected.database_id=DB_ID()
OPTION(MAXDOP 1,LOOP JOIN);
SELECT /* culprit */ 'entry' AS row_kind,CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 CONVERT(varchar(130),cp.plan_handle,1) AS plan_handle,
 CONVERT(varchar(130),CONVERT(varbinary(64),binding.native_value),1) AS sql_handle,
 cp.size_in_bytes,cp.cacheobjtype,cp.objtype,cp.usecounts,cp.refcounts
FROM @handles selected JOIN sys.dm_exec_cached_plans cp ON cp.plan_handle=selected.plan_handle
JOIN @attributes db ON db.plan_handle=selected.plan_handle AND db.name='dbid'
JOIN @attributes binding ON binding.plan_handle=selected.plan_handle AND binding.name='sql_handle'
WHERE CONVERT(int,db.native_value)=DB_ID() OPTION(MAXDOP 1,LOOP JOIN);
SELECT /* culprit */ 'key' AS row_kind,CONVERT(varchar(130),a.plan_handle,1) AS plan_handle,
 a.name AS attribute_name,a.native_type,
 CASE WHEN a.native_type IN('bit','tinyint','smallint','int','bigint') THEN CONVERT(nvarchar(512),a.native_value)
  WHEN a.native_type IN('binary','varbinary') AND DATALENGTH(CONVERT(varbinary(max),a.native_value))<=255
   THEN N'0x'+CONVERT(varchar(510),CONVERT(varbinary(max),a.native_value),2)
  WHEN a.native_type IN('char','varchar','nchar','nvarchar') AND DATALENGTH(CONVERT(nvarchar(max),a.native_value))<=256
   THEN CONVERT(nvarchar(128),a.native_value) END AS attribute_value
FROM @attributes a WHERE a.is_cache_key=1 OPTION(MAXDOP 1,LOOP JOIN);"#
    )
}
