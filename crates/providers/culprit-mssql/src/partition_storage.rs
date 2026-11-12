//! Partition row counts and allocation pages are native statistics, never reads of application contents.
pub(crate) const PAGE_BYTES: u64 = 8192;
pub(crate) fn relation(with_statistics: bool) -> &'static str {
    if with_statistics { PARTITION_STATISTICS } else { CATALOG_PARTITIONS }
}

const PARTITION_STATISTICS: &str = r#"
SELECT object_id,index_id,partition_id,CONVERT(decimal(38,0),row_count) AS row_count,
 CONVERT(decimal(38,0),reserved_page_count) AS reserved_pages,
 CONVERT(decimal(38,0),in_row_reserved_page_count) AS in_row_pages,
 CONVERT(decimal(38,0),lob_reserved_page_count) AS lob_pages,
 CONVERT(decimal(38,0),row_overflow_reserved_page_count) AS overflow_pages
FROM sys.dm_db_partition_stats"#;
// Allocation type 1/3 belongs to a rowstore HoBT; type 2 belongs to the partition.
// Columnstore delta-store HoBTs need different accounting, so the catalog-only projection leaves their sizes unknown.
// https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-allocation-units-transact-sql
const CATALOG_PARTITIONS: &str = r#"
SELECT p.object_id,p.index_id,p.partition_id,CONVERT(decimal(38,0),p.rows) AS row_count,
 SUM(CONVERT(decimal(38,0),COALESCE(au.total_pages,0))) AS reserved_pages,
 SUM(CASE WHEN au.type=1 THEN CONVERT(decimal(38,0),au.total_pages) ELSE 0 END) AS in_row_pages,
 SUM(CASE WHEN au.type=2 THEN CONVERT(decimal(38,0),au.total_pages) ELSE 0 END) AS lob_pages,
 SUM(CASE WHEN au.type=3 THEN CONVERT(decimal(38,0),au.total_pages) ELSE 0 END) AS overflow_pages
FROM sys.partitions p
LEFT JOIN sys.allocation_units au
 ON (au.type IN (1,3) AND au.container_id=p.hobt_id)
 OR (au.type=2 AND au.container_id=p.partition_id)
GROUP BY p.object_id,p.index_id,p.partition_id,p.rows"#;
