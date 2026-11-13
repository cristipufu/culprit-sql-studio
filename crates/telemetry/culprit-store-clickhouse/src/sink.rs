//! Native ClickHouse SQL and inserts adapt the shared analytical sink; delivery policy is never reimplemented.
use crate::{
    catalog::SchemaCatalog,
    client::{InsertData, QueryRequest, QueryTransport},
    response::Column,
    schema::quote,
};
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        ProjectionCatalog,
        encoding::EncodedBatch,
        indexes::ProjectionIndexes,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
        sink::{AnalyticalSink, AnalyticalStore},
    },
    delivery::{
        contract::Contract,
        object_store::transport::ObjectTransport,
        recovery::{CommittedRange, RecoveryStore},
        sink::{DeliveryAttempt, Submission},
    },
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub struct ClickHouseStore {
    catalog: Arc<SchemaCatalog>,
    query: Arc<dyn QueryTransport>,
}
impl ClickHouseStore {
    pub fn new(query: Arc<dyn QueryTransport>) -> Result<Self> {
        Ok(Self { catalog: Arc::new(SchemaCatalog::new()?), query })
    }
}
pub fn new(
    contract: Contract,
    recovery: Arc<dyn RecoveryStore>,
    validation: Arc<dyn ObjectTransport>,
    query: Arc<dyn QueryTransport>,
    scratch: &Path,
) -> Result<AnalyticalSink> {
    AnalyticalSink::new(
        contract,
        recovery,
        Arc::new(ProjectionIndexes::new(validation)),
        Arc::new(ClickHouseStore::new(query)?),
        scratch,
    )
}
#[async_trait]
impl AnalyticalReadStore for ClickHouseStore {
    fn destination(&self) -> &str {
        self.query.destination()
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(
        &self,
        stream: culprit_core::telemetry::TelemetryStream,
        row: &[Value],
    ) -> Result<serde_json::Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }

    async fn audit_membership(&self, range: &CommittedRange, byte_limit: usize) -> Result<usize> {
        let branches = self
            .catalog
            .schemas()
            .map(|(_, schema)| {
                format!(
                    "SELECT '{}' AS LandingTable, BatchId, RowOrdinal FROM {} WHERE RangeId={{range:String}}",
                    schema.table(),
                    quote(schema.table())
                )
            })
            .collect::<Vec<_>>();
        let mut choices = Vec::new();
        for batch in range.manifest().batches.iter().filter(|batch| batch.delivery_required) {
            let schema = self.catalog.get(batch.stream)?;
            choices.push(format!(
                "LandingTable='{}' AND BatchId='{}', toInt64({})",
                schema.table(),
                batch.batch_id,
                batch.rows.len()
            ));
        }
        let expected = if choices.is_empty() {
            "toInt64(-1)".into()
        } else {
            format!("multiIf({},toInt64(-1))", choices.join(","))
        };
        let text = format!(
            "SELECT countIf(RowOrdinal<=0 OR RowOrdinal>{expected}) AS InvalidRows FROM ({})",
            branches.join(" UNION ALL ")
        );
        let table = self
            .query
            .query(QueryRequest {
                text,
                parameters: range_parameter(range),
                columns: vec![Column { name: "InvalidRows".into(), kind: "UInt64".into() }],
                row_limit: 1,
                byte_limit,
            })
            .await?;
        if table.rows != vec![vec![json!(0)]] {
            return Err(invalid());
        }
        Ok(table.response_bytes)
    }
    async fn rows(
        &self,
        range: &CommittedRange,
        batch_id: &str,
        after: usize,
        until: usize,
        byte_limit: usize,
    ) -> Result<AnalyticalReadPage> {
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == batch_id).ok_or_else(invalid)?;
        let schema = self.catalog.get(batch.stream)?;
        let text = format!(
            "SELECT DISTINCT {} FROM {} WHERE RangeId={{range:String}} AND BatchId={{batch:String}} AND RowOrdinal>{{after:Int64}} AND RowOrdinal<={{until:Int64}} LIMIT {}",
            schema.verification_projection(),
            quote(schema.table()),
            until - after + 1
        );
        let mut parameters = range_parameter(range);
        parameters.extend([
            ("batch".into(), batch_id.into()),
            ("after".into(), after.to_string()),
            ("until".into(), until.to_string()),
        ]);
        let table = self
            .query
            .query(QueryRequest {
                text,
                parameters,
                columns: schema.verification_columns(),
                row_limit: until - after + 1,
                byte_limit,
            })
            .await?;
        Ok(AnalyticalReadPage { rows: table.rows, response_bytes: table.response_bytes })
    }
    async fn publication_rows(&self, range: &CommittedRange, byte_limit: usize) -> Result<AnalyticalReadPage> {
        let schema = self.catalog.publication();
        let text = format!(
            "SELECT DISTINCT {} FROM {} WHERE RangeId={{range:String}} LIMIT 2",
            schema.verification_projection(),
            quote(schema.table())
        );
        let table = self
            .query
            .query(QueryRequest {
                text,
                parameters: range_parameter(range),
                columns: schema.verification_columns(),
                row_limit: 2,
                byte_limit,
            })
            .await?;
        Ok(AnalyticalReadPage { rows: table.rows, response_bytes: table.response_bytes })
    }
}
#[async_trait]
impl AnalyticalStore for ClickHouseStore {
    fn polling_mode(&self) -> &'static str {
        "clickhouse-readback-v1"
    }
    async fn dispatch(&self, _: &CommittedRange, _: &DeliveryAttempt, batch: Arc<EncodedBatch>) -> Result<Submission> {
        let table = batch.table.clone();
        self.query.insert(&table, InsertData::Batch(batch)).await
    }
    async fn publish(&self, _: &CommittedRange, _: &DeliveryAttempt, _: &str, bytes: Vec<u8>) -> Result<()> {
        match self.query.insert(self.catalog.publication().table(), InsertData::Control(bytes)).await? {
            Submission::Accepted | Submission::Unknown => Ok(()),
            Submission::Rejected => Err(DeliveryProblem::Transport.error()),
        }
    }
}

fn range_parameter(range: &CommittedRange) -> BTreeMap<String, String> {
    BTreeMap::from([("range".into(), range.manifest().range_id.clone())])
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
