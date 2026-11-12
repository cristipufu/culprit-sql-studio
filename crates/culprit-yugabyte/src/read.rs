//! App projections use the same owned PostgreSQL-wire reader as background observations.
use culprit_core::limits::MAX_RESULT_ROWS as MAX_ROWS;
use culprit_core::{Engine, Result, telemetry::CollectionStop};
use culprit_postgres::wire::Connector;
pub(crate) use culprit_postgres::wire::{NoticeRead, QueryRead, ReadSession, Row};
use std::time::Duration;
pub(crate) fn application(connector: Connector, window: Duration) -> ReadSession {
    ReadSession::for_engine(connector, culprit_read::application_budget(window), Engine::Yugabyte)
}
pub(crate) async fn query(reader: &mut impl QueryRead, sql: &str) -> Result<Vec<Row>> {
    reader.query(sql, MAX_ROWS).await
}
pub(crate) async fn query_one(reader: &mut impl QueryRead, sql: &str) -> Result<Option<Row>> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() > 1 {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(rows.pop())
}
pub(crate) async fn query_bounded(reader: &mut impl QueryRead, sql: &str, limit: usize) -> Result<(Vec<Row>, bool)> {
    let mut rows = reader.query(sql, limit + 1).await?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, truncated))
}
