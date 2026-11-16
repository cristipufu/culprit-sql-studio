use super::Selection;

const QUEUED_PROJECTION: &str = "{queued-stalls}";
const COUNTERS: &str = "vfs.database_id, vfs.file_id, vfs.num_of_reads AS reads,
vfs.io_stall_read_ms AS read_ms, vfs.num_of_bytes_read AS read_bytes,
vfs.num_of_writes AS writes, vfs.io_stall_write_ms AS write_ms, vfs.num_of_bytes_written AS write_bytes,
vfs.size_on_disk_bytes, vfs.sample_ms, {queued-stalls}";
const CONTEXT: &str = "@engine_edition AS engine_edition, @database_edition AS database_edition,
@replica_id AS replica_id, @updateability AS updateability";
const SERIAL: &str = " OPTION(MAXDOP 1);";

pub(super) fn statement(selection: Selection) -> String {
    let current = dynamic_expression(&database());
    let cache = dynamic_expression(&cache());
    let tempdb = match selection {
        Selection::Database => String::new(),
        Selection::DatabaseAndSharedTempdb => format!(
            "
IF @engine_edition IN (2,3,4,8) AND DB_ID() <> 2
BEGIN
SET @statement = @statement + N' UNION ALL ' + {};
END;",
            dynamic_expression(&tempdb())
        ),
    };
    // The queued columns were added in SQL Server 2014. Keep them inside fixed dynamic SQL so SQL Server 2012
    // never compiles unavailable columns. Azure SQL Database and MI expose the documented fields independently
    // of their product major number. Unknown platforms retain the original counters with unknown queued stalls.
    // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-views/sys-dm-io-virtual-file-stats-transact-sql
    // ProductMajorVersion arrived in SQL Server 2014 RTM CU10 / SP1 CU3. Earlier 2014 builds still expose the
    // queued counters, so fall back to the documented major.minor.build.revision ProductVersion in this batch.
    // https://techcommunity.microsoft.com/t5/sql-server-blog/version-and-serverproperty-8211-what-changed/ba-p/384513
    format!(
        "DECLARE @engine_edition int = CONVERT(int, SERVERPROPERTY('EngineEdition'));
DECLARE @major int = COALESCE(
TRY_CONVERT(int, SERVERPROPERTY('ProductMajorVersion')),
TRY_CONVERT(int, PARSENAME(CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion')), 4)));
DECLARE @database_edition nvarchar(64) = CONVERT(nvarchar(64), DATABASEPROPERTYEX(DB_NAME(), 'Edition'));
DECLARE @replica_id nvarchar(36) = {replica_id};
DECLARE @updateability nvarchar(60) = CONVERT(nvarchar(60), DATABASEPROPERTYEX(DB_NAME(), 'Updateability'));
DECLARE @queued_counters nvarchar(256) = CASE
WHEN @engine_edition IN (5,8) OR (@engine_edition IN (2,3,4) AND @major >= 12)
THEN N'vfs.io_stall_queued_read_ms AS queued_read_ms, vfs.io_stall_queued_write_ms AS queued_write_ms'
ELSE N'CAST(NULL AS bigint) AS queued_read_ms, CAST(NULL AS bigint) AS queued_write_ms' END;
DECLARE @current nvarchar(max) = {current};
DECLARE @statement nvarchar(max);
IF @engine_edition = 5 AND (@database_edition = N'Hyperscale' OR @replica_id IS NOT NULL)
BEGIN
SET @statement = @current + N' UNION ALL ' + {cache} + N'{SERIAL}';
END
ELSE
BEGIN
SET @statement = @current;{tempdb}
SET @statement = @statement + N'{SERIAL}';
END;
EXEC sys.sp_executesql @statement,
N'@engine_edition int, @database_edition nvarchar(64), @replica_id nvarchar(36), @updateability nvarchar(60)',
@engine_edition = @engine_edition, @database_edition = @database_edition,
@replica_id = @replica_id, @updateability = @updateability;",
        replica_id = crate::replica::ID_EXPRESSION,
    )
}

fn database() -> String {
    format!(
        "/* culprit */ SELECT {COUNTERS}, DB_NAME() AS database_name, df.name AS file_name,
CAST(df.type AS varchar(16)) AS native_type, df.size AS logical_pages, df.max_size AS maximum_pages,
CAST(0 AS int) AS shared, CAST(0 AS int) AS cache,
CASE WHEN @engine_edition IN (2,3,4,8) AND DB_ID() = 2 THEN 1 ELSE 0 END AS temporary,
{CONTEXT}
FROM sys.dm_io_virtual_file_stats(DB_ID(), NULL) vfs
LEFT JOIN sys.database_files df ON df.file_id = vfs.file_id
WHERE vfs.database_id = DB_ID()"
    )
}

fn tempdb() -> String {
    // master_files.size is the startup template for tempdb. The separately selected shared read needs the
    // current database catalog instead. This literal is compiled only in the non-Hyperscale native branch.
    // https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-master-files-transact-sql
    format!(
        "/* culprit */ SELECT {COUNTERS}, N'tempdb' AS database_name, tf.name AS file_name,
CAST(tf.type AS varchar(16)) AS native_type, tf.size AS logical_pages, tf.max_size AS maximum_pages,
CAST(1 AS int) AS shared, CAST(0 AS int) AS cache, CAST(1 AS int) AS temporary, {CONTEXT}
FROM sys.dm_io_virtual_file_stats(2, NULL) vfs
LEFT JOIN tempdb.sys.database_files tf ON tf.file_id = vfs.file_id
WHERE vfs.database_id = 2 AND DB_ID() <> 2"
    )
}

fn cache() -> String {
    // database_id=0 is a Hyperscale-only interface. A WHERE predicate is not an execution guard.
    format!(
        "/* culprit */ SELECT {COUNTERS}, CAST(NULL AS nvarchar(128)) AS database_name,
CAST(NULL AS nvarchar(128)) AS file_name, CAST(NULL AS varchar(16)) AS native_type,
CAST(NULL AS int) AS logical_pages, CAST(NULL AS int) AS maximum_pages,
CAST(0 AS int) AS shared, CAST(1 AS int) AS cache, CAST(0 AS int) AS temporary, {CONTEXT}
FROM sys.dm_io_virtual_file_stats(0, NULL) vfs WHERE vfs.database_id = 0"
    )
}

/// Only provider-owned literals enter the statement; no native names or caller values become SQL syntax.
fn dynamic_expression(sql: &str) -> String {
    let (before, after) = sql.split_once(QUEUED_PROJECTION).expect("one fixed queued-stall projection");
    format!("N'{}' + @queued_counters + N'{}'", before.replace('\'', "''"), after.replace('\'', "''"))
}

#[cfg(test)]
mod tests;
