//! Scripted native ownership for source recipes; every unexpected read is a test failure.
use super::{OwnedRead, QueryRead};
use crate::rows::Rec;
use culprit_core::{Result, telemetry::*};
use mysql_async::Value;
use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct Read {
    pub replies: VecDeque<Result<Vec<Rec>>>,
    pub queries: Vec<(String, Vec<Value>, usize)>,
    pub closed: usize,
    pub shutdown: Option<SourceShutdown>,
}

impl Read {
    pub fn with(replies: Vec<Result<Vec<Rec>>>) -> Self {
        Self { replies: replies.into(), ..Default::default() }
    }
}

#[async_trait::async_trait]
impl QueryRead for Read {
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>> {
        self.queries.push((sql.into(), parameters, row_limit));
        self.replies.pop_front().expect("unexpected source SQL")
    }
    async fn explain(&mut self, _: &str) -> Result<Option<Rec>> {
        panic!("metadata acquisition must never explain application SQL")
    }
}

#[async_trait::async_trait]
impl OwnedRead for Read {
    fn query_count(&self) -> u32 {
        self.queries.len() as u32
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed += 1;
        self.shutdown.unwrap_or(SourceShutdown::Quiescent)
    }
}
