//! App projections use the same owned PostgreSQL-wire reader as background observations.
use culprit_core::{Engine, Result, telemetry::CollectionBudget};
use culprit_postgres::wire::Connector;
pub(crate) use culprit_postgres::wire::{NoticeRead, QueryRead, ReadSession, Row};
use std::time::Duration;
const MAX_ROWS: usize = 10_000;
pub(crate) fn application(connector: Connector, window: Duration) -> ReadSession {
    ReadSession::for_engine(
        connector,
        CollectionBudget {
            max_snapshot_rows: MAX_ROWS as u32,
            max_queries: 512,
            page_bytes: 8 * 1024 * 1024,
            statement_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
            total_timeout_ms: (window.as_millis() + 60_000).min(900_000) as u32,
            ..Default::default()
        },
        Engine::Yugabyte,
    )
}
pub(crate) async fn query(reader: &mut impl QueryRead, sql: &str) -> Result<Vec<Row>> {
    reader.query(sql, MAX_ROWS).await
}
pub(crate) async fn query_one(reader: &mut impl QueryRead, sql: &str) -> Result<Option<Row>> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() > 1 {
        return Err(crate::native::invalid());
    }
    Ok(rows.pop())
}
pub(crate) async fn query_bounded(reader: &mut impl QueryRead, sql: &str, limit: usize) -> Result<(Vec<Row>, bool)> {
    let mut rows = reader.query(sql, limit + 1).await?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, truncated))
}
