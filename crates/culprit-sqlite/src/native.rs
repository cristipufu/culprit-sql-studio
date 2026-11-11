//! Native SQL is shared by app projections and bounded export. No page scan or application statement execution.
pub(crate) mod state;
use culprit_core::{Result, telemetry::*};
use rusqlite::{Connection, Params, types::ValueRef};
use std::{collections::BTreeMap, sync::atomic::Ordering};

pub(crate) const SCHEMA: &str = "/* culprit */ SELECT s.type, s.name, s.sql, t.type, t.wr, s.tbl_name, s.rootpage
 FROM main.sqlite_schema s LEFT JOIN pragma_table_list t ON t.schema='main' AND t.name=s.name AND s.type='table'
 WHERE s.type IN ('table','index','view','trigger') AND s.name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
 AND coalesce(t.type,'') <> 'shadow' ORDER BY s.name";
pub(crate) const COLUMNS: &str =
    "/* culprit */ SELECT cid,name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo(?1)";
pub(crate) const INDEXES: &str = "/* culprit */ SELECT name,\"unique\",origin,partial FROM pragma_index_list(?1)";
pub(crate) const INDEX_KEYS: &str =
    "/* culprit */ SELECT seqno,name,\"desc\",coll,cid,key FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno";
pub(crate) const FOREIGN_KEYS: &str = "/* culprit */ SELECT id,seq,\"table\",\"from\",\"to\",on_delete,on_update,match FROM pragma_foreign_key_list(?1) ORDER BY id,seq";
pub(crate) const STATISTICS: &str = "/* culprit */ SELECT tbl,idx,stat FROM main.sqlite_stat1";
pub(crate) const FILE_PRAGMAS: &[&str] =
    &["journal_mode", "auto_vacuum", "page_size", "page_count", "freelist_count", "encoding"];
pub(crate) type NativeRow = Vec<Option<String>>;
pub(crate) fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

pub(crate) struct CatalogReader<'a> {
    conn: &'a Connection,
    budget: &'a CollectionBudget,
    bytes: usize,
    rows: usize,
    accounting: crate::read::FileAccounting,
}
impl<'a> CatalogReader<'a> {
    pub(crate) fn new(
        conn: &'a Connection,
        budget: &'a CollectionBudget,
        accounting: crate::read::FileAccounting,
    ) -> Self {
        Self { conn, budget, bytes: 0, rows: 0, accounting }
    }
    #[cfg(test)]
    fn accounting(&self) -> (u32, Option<ReadWorkMetrics>) {
        self.accounting.snapshot()
    }
    pub(crate) fn query(&mut self, sql: &str, params: impl Params) -> Result<Vec<NativeRow>> {
        let progress = ReadProgress::default();
        self.accounting.clone().statement(sql, &progress, || {
            let mut statement = self.conn.prepare(sql).map_err(|error| crate::map_error(&error))?;
            let width = statement.column_count();
            let mut cursor = statement.query(params).map_err(|error| crate::map_error(&error))?;
            let mut rows = vec![];
            while let Some(row) = cursor.next().map_err(|error| crate::map_error(&error))? {
                let mut values = vec![];
                for index in 0..width {
                    let value = match row.get_ref(index).map_err(|error| crate::map_error(&error))? {
                        ValueRef::Null => None,
                        ValueRef::Integer(value) => Some(value.to_string()),
                        ValueRef::Real(value) if value.is_finite() => Some(value.to_string()),
                        ValueRef::Text(value) => {
                            if value.len() > (self.budget.total_bytes as usize).saturating_sub(self.bytes) {
                                return Err(CollectionStop::ByteLimit.error());
                            }
                            Some(std::str::from_utf8(value).map_err(|_| invalid())?.to_string())
                        }
                        _ => return Err(invalid()),
                    };
                    let bytes = value.as_ref().map_or(1, String::len);
                    self.bytes = self.bytes.checked_add(bytes).ok_or_else(invalid)?;
                    progress.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                    if self.bytes > self.budget.total_bytes as usize {
                        return Err(CollectionStop::ByteLimit.error());
                    }
                    values.push(value);
                }
                self.rows += 1;
                progress.rows.fetch_add(1, Ordering::Relaxed);
                if self.rows > self.budget.max_snapshot_rows as usize {
                    return Err(CollectionStop::RowLimit.error());
                }
                rows.push(values);
            }
            progress.drained.store(true, Ordering::Relaxed);
            Ok(rows)
        })
    }
}
pub(crate) fn properties(row: &NativeRow, fields: &[(usize, &str)]) -> BTreeMap<String, String> {
    fields
        .iter()
        .filter_map(|(i, name)| row.get(*i).and_then(Clone::clone).map(|value| (name.to_string(), value)))
        .collect()
}

pub(crate) fn wal_bytes(path: &std::path::Path) -> Result<u64> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    match std::fs::metadata(std::path::PathBuf::from(wal)) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(_) => Err(culprit_core::Error::internal(crate::collect::wal_unreadable())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_limits_cover_the_whole_read_and_record_failures_without_partial_results() {
        let conn = Connection::open_in_memory().unwrap();
        let cases = [
            (
                CollectionBudget { total_bytes: 4, ..Default::default() },
                "/* culprit */ SELECT 'private-value'",
                CollectionStop::ByteLimit,
            ),
            (
                CollectionBudget { max_snapshot_rows: 1, ..Default::default() },
                "/* culprit */ SELECT 1 UNION ALL SELECT 2",
                CollectionStop::RowLimit,
            ),
        ];
        for (budget, sql, stop) in cases {
            let mut reader = CatalogReader::new(&conn, &budget, crate::read::FileAccounting::new(budget.max_queries));
            assert_eq!(reader.query(sql, []).unwrap_err().detail, stop.error().detail);
            let (queries, metrics) = reader.accounting();
            assert_eq!(queries, 1);
            let metrics = metrics.unwrap();
            assert_eq!(metrics.active_tasks, 0);
            assert_eq!(metrics.templates[0].failures, 1);
            assert!(!serde_json::to_string(&metrics).unwrap().contains("private-value"));
        }
        let budget = CollectionBudget { max_queries: 1, ..Default::default() };
        let mut reader = CatalogReader::new(&conn, &budget, crate::read::FileAccounting::new(budget.max_queries));
        reader.query("/* culprit */ SELECT 1", []).unwrap();
        assert_eq!(
            reader.query("/* culprit */ SELECT 2", []).unwrap_err().detail,
            CollectionStop::QueryLimit.error().detail
        );
        assert_eq!(reader.accounting().0, 1);
    }
}
