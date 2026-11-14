//! Native column discovery keeps optional completed-execution counters out of older view projections.
use super::*;
use crate::read::{NativeRead, text};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Layout {
    spilled_pages: bool,
}

impl Layout {
    pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let names = STATISTICS.map(|statistic| format!("{statistic}_spills"));
        let selected = names.iter().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT name FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_exec_query_stats') AND name IN ({selected}) OPTION(MAXDOP 1)"
        );
        let rows =
            reader.query(&sql, vec![], STATISTICS.len() + 1).await.map_err(crate::read::ReadError::into_error)?;
        let mut found = BTreeSet::new();
        for row in &rows {
            let name = text(row, "name")?;
            if !names.contains(&name) || !found.insert(name) {
                return Err(invalid_data());
            }
        }
        if !found.is_empty() && found.len() != STATISTICS.len() {
            return Err(invalid_data());
        }
        Ok(Self { spilled_pages: !found.is_empty() })
    }

    pub(crate) fn supports(&self, metric: CachedRuntimeMetric) -> bool {
        metric.is_required() || self.spilled_pages
    }
}
