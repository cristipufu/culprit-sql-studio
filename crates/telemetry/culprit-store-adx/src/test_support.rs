use crate::{
    client::{QueryRequest, QueryTransport},
    response::QueryTable,
};
use async_trait::async_trait;
pub(crate) use culprit_collector::analytical::test_support::{
    Blobs, Fault, Recovery, activity, cumulative_queries, fixture, interval_queries, waits,
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;

pub(super) enum Reply {
    Audit(u64),
    Rows(Vec<Vec<Value>>),
    Failure,
}
#[derive(Default)]
pub(super) struct Queries {
    pub(super) replies: Mutex<VecDeque<Reply>>,
    pub(super) requests: Mutex<Vec<QueryRequest>>,
}
#[async_trait]
impl QueryTransport for Queries {
    fn destination(&self) -> &str {
        "adx:test"
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected query");
        let columns = request.columns.clone();
        self.requests.lock().unwrap().push(request);
        match reply {
            Reply::Audit(count) => Ok(QueryTable { columns, rows: vec![vec![json!(count)]] }),
            Reply::Rows(rows) => Ok(QueryTable { columns, rows }),
            Reply::Failure => Err(DeliveryProblem::Transport.error()),
        }
    }
}
