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
    pub explains: Vec<String>,
    query_limit: Option<u32>,
    initial_queries: u32,
    native_phases: bool,
    charged_phases: u32,
}

impl Read {
    pub fn with(replies: Vec<Result<Vec<Rec>>>) -> Self {
        Self { replies: replies.into(), ..Default::default() }
    }
    pub fn with_query_budget(mut self, limit: u32, initial_queries: u32) -> Self {
        self.query_limit = Some(limit);
        self.initial_queries = initial_queries;
        self
    }
    /// Native parameterized reads charge PREPARE and EXECUTE separately, including a cached PREPARE attempt.
    pub fn with_native_phases(mut self) -> Self {
        self.native_phases = true;
        self
    }
    fn charge(&mut self, phases: u32) -> Result<()> {
        for _ in 0..phases {
            if self.query_limit.is_some_and(|limit| self.query_count() >= limit) {
                return Err(CollectionStop::QueryLimit.error());
            }
            if self.native_phases {
                self.charged_phases += 1;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl QueryRead for Read {
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>> {
        let phases = if self.native_phases && !parameters.is_empty() { 2 } else { 1 };
        self.charge(phases)?;
        self.queries.push((sql.into(), parameters, row_limit));
        self.replies.pop_front().expect("unexpected source SQL")
    }
    async fn explain(&mut self, sql: &str) -> Result<Option<Rec>> {
        self.charge(if self.native_phases { 2 } else { 1 })?;
        self.explains.push(sql.into());
        let rows = self.replies.pop_front().expect("unexpected native plan read")?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(rows.into_iter().next()),
            _ => Err(CollectionStop::InvalidPage.error()),
        }
    }
}

#[async_trait::async_trait]
impl OwnedRead for Read {
    fn query_count(&self) -> u32 {
        self.initial_queries
            + if self.native_phases { self.charged_phases } else { (self.queries.len() + self.explains.len()) as u32 }
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed += 1;
        self.shutdown.unwrap_or(SourceShutdown::Quiescent)
    }
}
