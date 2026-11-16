use super::*;

fn outside_literals(sql: &str) -> String {
    let mut chars = sql.chars().peekable();
    let mut output = String::new();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if quoted && chars.peek() == Some(&'\'') {
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if !quoted {
            output.push(ch);
        }
    }
    assert!(!quoted, "every fixed native SQL literal is closed");
    output
}

#[test]
fn older_sql_server_and_unknown_editions_never_compile_the_new_optional_columns() {
    let sql = statement(Selection::Database);
    assert!(sql.contains("TRY_CONVERT(int, SERVERPROPERTY('ProductMajorVersion'))"));
    assert!(sql.contains("@engine_edition IN (5,8) OR (@engine_edition IN (2,3,4) AND @major >= 12)"));
    assert!(sql.contains("ELSE N'CAST(NULL AS bigint) AS queued_read_ms, CAST(NULL AS bigint) AS queued_write_ms'"));
    let outer = outside_literals(&sql);
    assert!(!outer.contains("vfs.io_stall_queued"), "SQL Server 2012 must not bind absent native columns");
    assert_eq!(outer.matches("EXEC sys.sp_executesql @statement").count(), 1);
    assert!(!outer.contains("FROM sys."), "the compatibility decision does not add a metadata query");
}

#[test]
fn early_sql_server_2014_uses_the_native_product_version_without_another_query_or_unconditional_projection() {
    let sql = statement(Selection::Database);
    let fallback = "DECLARE @major int = COALESCE(\n\
TRY_CONVERT(int, SERVERPROPERTY('ProductMajorVersion')),\n\
TRY_CONVERT(int, PARSENAME(CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion')), 4)));";
    assert!(sql.contains(fallback), "RTM 2014 needs the four-part native version when ProductMajorVersion is absent");
    assert_eq!(sql.matches("SERVERPROPERTY('ProductVersion')").count(), 1);
    assert!(sql.find(fallback).unwrap() < sql.find("DECLARE @queued_counters").unwrap());
    assert!(sql.contains("(@engine_edition IN (2,3,4) AND @major >= 12)"));
    assert!(sql.contains("ELSE N'CAST(NULL AS bigint) AS queued_read_ms, CAST(NULL AS bigint) AS queued_write_ms'"));
    let outer = outside_literals(&sql);
    assert!(!outer.contains("SELECT"), "both version properties share the bounded native batch declarations");
    assert_eq!(outer.matches("EXEC sys.sp_executesql @statement").count(), 1);
}

#[test]
fn targeted_database_cache_and_shared_tempdb_templates_are_tagged_and_compile_only_when_selected() {
    for (selection, shared) in [(Selection::Database, false), (Selection::DatabaseAndSharedTempdb, true)] {
        let sql = statement(selection);
        let outer = outside_literals(&sql);
        assert!(!outer.contains("tempdb.sys.database_files"), "Azure SQL cannot bind cross-database metadata");
        assert!(!outer.contains("sys.dm_io_virtual_file_stats"));
        assert_eq!(sql.contains("LEFT JOIN tempdb.sys.database_files"), shared);
        assert!(!sql.contains("sys.master_files"), "tempdb startup size is not its current size");
        assert!(!sql.contains("sys.dm_io_virtual_file_stats(NULL"));
        let branch = sql.find("IF @engine_edition = 5").unwrap();
        let otherwise = sql[branch..].find("ELSE").unwrap() + branch;
        let cache_call = sql.find("FROM sys.dm_io_virtual_file_stats(0, NULL)").unwrap();
        assert!(branch < cache_call && cache_call < otherwise, "only the positive Hyperscale branch compiles RBPEX");
        if shared {
            let shared_call = sql.find("FROM sys.dm_io_virtual_file_stats(2, NULL)").unwrap();
            let shared_guard = sql.find("IF @engine_edition IN (2,3,4,8) AND DB_ID() <> 2").unwrap();
            assert!(otherwise < shared_guard && shared_guard < shared_call);
            assert!(sql.contains("WHERE vfs.database_id = 2 AND DB_ID() <> 2"));
        }
        assert!(database().starts_with("/* culprit */ SELECT"));
        assert!(tempdb().starts_with("/* culprit */ SELECT"));
        assert!(cache().starts_with("/* culprit */ SELECT"));
        assert!(sql.contains("OPTION(MAXDOP 1)"));
        assert!(sql.contains("df.size AS logical_pages, df.max_size AS maximum_pages"));
        assert!(sql.contains("CAST(NULL AS int) AS logical_pages, CAST(NULL AS int) AS maximum_pages"));
    }
}
