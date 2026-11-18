//! Native Query Store schema facts are decoded once for scans and bounded captured history.
use crate::{
    read::{integer, invalid_data},
    value::Rec,
};
use culprit_core::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeLayout {
    pub(crate) edition: i64,
    pub(crate) tempdb: bool,
    pub(crate) log_bytes: bool,
    pub(crate) page_server_reads: super::page_server::Availability,
    pub(crate) waits: bool,
    pub(crate) variants: super::variants_sql::Availability,
    pub(crate) mode: super::mode::ReadMode,
    pub(crate) partitioned: bool,
}
impl NativeLayout {
    pub(crate) fn decode(row: &Rec) -> Result<Self> {
        let flag = |name| match integer(row, name)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_data()),
        };
        let mode =
            super::mode::ReadMode::from_native(&crate::read::text(row, "actual_state_desc")?).ok_or_else(|| {
                culprit_core::Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("collection.query_store_unavailable"),
                )
            })?;
        Ok(Self {
            edition: integer(row, "engine_edition")?,
            tempdb: flag("tempdb")?,
            log_bytes: flag("log_bytes")?,
            page_server_reads: super::page_server::decode_availability(row)?,
            waits: super::window_sql::waits_supported(row)?,
            variants: super::variants_sql::Availability::decode(row, "variants")?,
            mode,
            partitioned: flag("replicas")?,
        })
    }
}
macro_rules! columns {
    () => { concat!(
    r#"CAST(SERVERPROPERTY('EngineEdition') AS int) AS engine_edition,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_tempdb_space_used') THEN 1 ELSE 0 END AS tempdb,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_log_bytes_used') THEN 1 ELSE 0 END AS log_bytes,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'replica_group_id') THEN 1 ELSE 0 END AS replicas,
 "#,
    super::page_server::availability!(),
    " AS page_server_read_availability,",
    super::variants_sql::readable!(),
    r#" AS variants,
 CASE WHEN OBJECT_ID(N'sys.query_store_wait_stats') IS NOT NULL AND EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.database_query_store_options') AND name=N'wait_stats_capture_mode_desc') THEN 1 ELSE 0 END AS waits"#
) };
}
pub(crate) use columns;
