//! App projections use the same owned PostgreSQL-wire reader as background observations.
use culprit_core::Engine;
use culprit_postgres::wire::Connector;
pub(crate) use culprit_postgres::wire::{
    NoticeRead, QueryRead, ReadSession, Row, read_query as query, read_query_bounded as query_bounded,
    read_query_one as query_one,
};
use std::time::Duration;
pub(crate) fn application(connector: Connector, window: Duration) -> ReadSession {
    ReadSession::for_engine(connector, culprit_read::application_budget(window), Engine::Yugabyte)
}
