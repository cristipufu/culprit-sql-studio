use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_boolean},
    value::Rec,
};
use MssqlIndexCounter::*;
use culprit_core::{Result, telemetry::MssqlIndexCounter};
use std::collections::BTreeSet;
const RELATION: &str = "sys.dm_db_index_operational_stats";
pub(super) const COUNTERS: &[(MssqlIndexCounter, &str)] = &[
    (LeafGhosts, "leaf_ghost_count"),
    (NonleafInserts, "nonleaf_insert_count"),
    (NonleafDeletes, "nonleaf_delete_count"),
    (NonleafUpdates, "nonleaf_update_count"),
    (LeafPageMerges, "leaf_page_merge_count"),
    (NonleafPageMerges, "nonleaf_page_merge_count"),
    (LobFetchPages, "lob_fetch_in_pages"),
    (LobFetchBytes, "lob_fetch_in_bytes"),
    (LobOrphansCreated, "lob_orphan_create_count"),
    (LobOrphansInserted, "lob_orphan_insert_count"),
    (RowOverflowFetchPages, "row_overflow_fetch_in_pages"),
    (RowOverflowFetchBytes, "row_overflow_fetch_in_bytes"),
    (ValuesPushedOffRow, "column_value_push_off_row_count"),
    (ValuesPulledInRow, "column_value_pull_in_row_count"),
    (RowLocks, "row_lock_count"),
    (PageLocks, "page_lock_count"),
    (PageCompressionAttempts, "page_compression_attempt_count"),
    (PageCompressions, "page_compression_success_count"),
    (VersionsGeneratedInRow, "version_generated_inrow"),
    (VersionsGeneratedOffRow, "version_generated_offrow"),
    (GhostVersionsInRow, "ghost_version_inrow"),
    (GhostVersionsOffRow, "ghost_version_offrow"),
    (InsertOverGhostVersionsInRow, "insert_over_ghost_version_inrow"),
    (InsertOverGhostVersionsOffRow, "insert_over_ghost_version_offrow"),
    (CompactionAttempts, "compaction_attempt_count"),
    (CompactionsCompleted, "compaction_complete_count"),
    (CompactionsSkipped, "compaction_skip_count"),
    (CompactionsIneligible, "compaction_ineligible_count"),
    (CompactionFailures, "compaction_failure_count"),
    (CompactionRowsMoved, "compaction_row_move_count"),
    (CompactionPagesDeallocated, "compaction_page_deallocation_count"),
    (LeafInserts, "leaf_insert_count"),
    (LeafUpdates, "leaf_update_count"),
    (LeafDeletes, "leaf_delete_count"),
    (LeafAllocations, "leaf_allocation_count"),
    (NonleafAllocations, "nonleaf_allocation_count"),
    (RangeScans, "range_scan_count"),
    (SingletonLookups, "singleton_lookup_count"),
    (ForwardedFetches, "forwarded_fetch_count"),
    (RowLockWaits, "row_lock_wait_count"),
    (RowLockWaitMs, "row_lock_wait_in_ms"),
    (PageLockWaits, "page_lock_wait_count"),
    (PageLockWaitMs, "page_lock_wait_in_ms"),
    (PageLatchWaits, "page_latch_wait_count"),
    (PageLatchWaitMs, "page_latch_wait_in_ms"),
    (PageIoLatchWaits, "page_io_latch_wait_count"),
    (PageIoLatchWaitMs, "page_io_latch_wait_in_ms"),
    (TreePageLatchWaits, "tree_page_latch_wait_count"),
    (TreePageLatchWaitMs, "tree_page_latch_wait_in_ms"),
    (TreePageIoLatchWaits, "tree_page_io_latch_wait_count"),
    (TreePageIoLatchWaitMs, "tree_page_io_latch_wait_in_ms"),
    (LockEscalations, "index_lock_promotion_count"),
    (LockEscalationAttempts, "index_lock_promotion_attempt_count"),
];
#[derive(Clone)]
pub(crate) struct Layout {
    columns: BTreeSet<String>,
    sequential_option: bool,
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let presence=COUNTERS.iter().map(|(_,column)|*column).chain(["database_id","object_id","index_id","partition_number","hobt_id"]).map(|column|format!("CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{RELATION}') AND name=N'{column}') THEN 1 ELSE 0 END AS {column}")).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility, CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.indexes') AND name=N'optimize_for_sequential_key') THEN 1 ELSE 0 END AS sequential_option,{presence} OPTION(MAXDOP 1)"
        );
        let rows = reader.query(&sql, vec![], 2).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        Self::decode(row)
    }
    fn decode(row: &Rec) -> Result<Self> {
        let mut columns = BTreeSet::new();
        for column in COUNTERS.iter().map(|(_, column)| *column).chain([
            "database_id",
            "object_id",
            "index_id",
            "partition_number",
            "hobt_id",
        ]) {
            match integer(row, column)? {
                1 => {
                    columns.insert(column.into());
                }
                0 => {}
                _ => return Err(invalid_data()),
            }
        }
        if !["database_id", "object_id", "index_id", "partition_number"]
            .into_iter()
            .all(|column| columns.contains(column))
        {
            if optional_boolean(row, "metadata_visibility")? != Some(true) {
                return Err(culprit_core::Error::new(
                    culprit_core::ErrorCode::PermissionDenied,
                    culprit_core::t!("provider.mssql.index_operations.metadata_unavailable"),
                )
                .with_hint("GRANT VIEW DEFINITION TO [<user>];"));
            }
            return Err(invalid_data());
        }
        Ok(Self { columns, sequential_option: optional_boolean(row, "sequential_option")?.ok_or_else(invalid_data)? })
    }
    pub fn for_application(mut self) -> Self {
        self.columns.retain(|column| {
            matches!(
                column.as_str(),
                "database_id"
                    | "object_id"
                    | "index_id"
                    | "partition_number"
                    | "hobt_id"
                    | "forwarded_fetch_count"
                    | "leaf_insert_count"
                    | "row_lock_wait_count"
                    | "row_lock_wait_in_ms"
                    | "page_lock_wait_count"
                    | "page_lock_wait_in_ms"
                    | "page_latch_wait_count"
                    | "page_latch_wait_in_ms"
                    | "index_lock_promotion_count"
                    | "index_lock_promotion_attempt_count"
            )
        });
        self
    }
    pub fn counters(&self) -> impl Iterator<Item = (MssqlIndexCounter, &'static str)> + '_ {
        COUNTERS.iter().copied().filter(|(_, column)| self.columns.contains(*column))
    }
}
pub(crate) fn select_sql(layout: &Layout) -> String {
    let fields = layout.counters().map(|(_, column)| format!("s.{column}")).collect::<Vec<_>>();
    let hobt = if layout.columns.contains("hobt_id") { "s.hobt_id" } else { "CAST(NULL AS bigint)" };
    let sequential =
        if layout.sequential_option { "CONVERT(int,i.optimize_for_sequential_key)" } else { "CAST(NULL AS int)" };
    let counters = if fields.is_empty() { String::new() } else { format!(",{}", fields.join(",")) };
    format!(
        r#"SELECT TOP(@P1) s.database_id,s.object_id,s.index_id,s.partition_number,{hobt} AS hobt_id,
 sch.name AS schema_name,o.name AS object_name,i.name AS index_name,RTRIM(o.type) AS object_type,
 CONVERT(int,o.is_ms_shipped) AS engine_internal,CONVERT(int,c.is_identity) AS leading_identity,
 CASE WHEN DATALENGTH(d.definition)<=8192 THEN d.definition ELSE NULL END AS leading_default,
 {sequential} AS optimize_sequential{counters}
 FROM sys.dm_db_index_operational_stats(DB_ID(),NULL,NULL,NULL) s
 LEFT JOIN sys.objects o ON o.object_id=s.object_id
 LEFT JOIN sys.schemas sch ON sch.schema_id=o.schema_id
 LEFT JOIN sys.indexes i ON i.object_id=s.object_id AND i.index_id=s.index_id
 LEFT JOIN sys.index_columns ic ON ic.object_id=s.object_id AND ic.index_id=s.index_id AND ic.key_ordinal=1
 LEFT JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
 LEFT JOIN sys.default_constraints d ON d.object_id=c.default_object_id
 WHERE s.database_id=DB_ID() OPTION(MAXDOP 1)"#
    )
}
