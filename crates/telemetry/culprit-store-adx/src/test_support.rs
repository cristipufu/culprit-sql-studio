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

pub(crate) fn event_grid_sink(
    contract: culprit_collector::delivery::contract::Contract,
    recovery: std::sync::Arc<dyn culprit_collector::delivery::recovery::RecoveryStore>,
    blobs: std::sync::Arc<Blobs>,
    query: std::sync::Arc<dyn QueryTransport>,
    scratch: &std::path::Path,
) -> Result<culprit_collector::analytical::sink::AnalyticalSink> {
    crate::sink::new(
        contract,
        recovery,
        blobs.clone(),
        std::sync::Arc::new(crate::ready::ReadyDispatcher::new(blobs)),
        query,
        scratch,
    )
}

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
        let rows = match reply {
            Reply::Audit(count) => vec![vec![json!(count)]],
            Reply::Rows(rows) => rows,
            Reply::Failure => return Err(DeliveryProblem::Transport.error()),
        };
        Ok(QueryTable { columns, response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
}

pub(crate) fn returned(schema: &crate::schema::Schema, row: &Value) -> Vec<Value> {
    schema
        .columns()
        .iter()
        .map(|column| {
            let value = &row[&column.name];
            match column.kind {
                crate::projection::ScalarType::JsonContainer => {
                    value.as_str().map_or_else(|| json!(""), |text| json!(text))
                }
                crate::projection::ScalarType::ExactCounter => json!(value.as_str().unwrap_or_default()),
                _ => value.clone(),
            }
        })
        .collect()
}
