//! KQL membership audit and native typed row queries. Paging and receipt policy live in culprit-collector.
use crate::{
    catalog::SchemaCatalog,
    client::{QueryRequest, QueryTransport},
    response::Column,
    schema::Schema,
};
use culprit_collector::delivery::recovery::CommittedRange;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub struct AdxReadback {
    query: Arc<dyn QueryTransport>,
    catalog: Arc<SchemaCatalog>,
}
impl AdxReadback {
    pub fn new(query: Arc<dyn QueryTransport>, catalog: Arc<SchemaCatalog>) -> Self {
        Self { query, catalog }
    }
    pub async fn rows(
        &self,
        range: &CommittedRange,
        batch_id: &str,
        after: usize,
        until: usize,
    ) -> Result<Vec<Vec<Value>>> {
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == batch_id).ok_or_else(invalid)?;
        let schema = self.catalog.get(batch.stream)?;
        let table = self.query.query(page_query(schema, &range.manifest().range_id, batch_id, after, until)).await?;
        if table.columns != schema.verification_columns() {
            return Err(invalid());
        }
        Ok(table.rows)
    }
    pub async fn audit_membership(&self, range: &CommittedRange) -> Result<()> {
        // Check every landing table, including families absent from this capture. A foreign batch or ordinal
        // cannot hide outside the expected per-batch value pages. This count is only an envelope guard;
        // every admitted typed value is separately compared with its immutable expected projection.
        let branches = self
            .catalog
            .schemas()
            .map(|(_, schema)| {
                format!(
                    "({} | where RangeId == pRange | project LandingTable=\"{}\", BatchId, RowOrdinal)",
                    schema.table(),
                    schema.table()
                )
            })
            .collect::<Vec<_>>();
        let mut choices = Vec::new();
        for batch in range.manifest().batches.iter().filter(|batch| batch.delivery_required) {
            let schema = self.catalog.get(batch.stream)?;
            choices.push(format!(
                "LandingTable == \"{}\" and BatchId == \"{}\", {}",
                schema.table(),
                batch.batch_id,
                batch.rows.len()
            ));
        }
        let expected =
            if choices.is_empty() { "long(-1)".into() } else { format!("case({}, long(-1))", choices.join(", ")) };
        let text = format!(
            "declare query_parameters(pRange:string);\nunion {}\n| extend ExpectedRows={}\n| summarize InvalidRows=countif(isnull(RowOrdinal) or RowOrdinal <= 0 or RowOrdinal > ExpectedRows)",
            branches.join(",\n"),
            expected
        );
        let columns = vec![Column { column_name: "InvalidRows".into(), column_type: "long".into() }];
        let table = self
            .query
            .query(QueryRequest {
                text,
                parameters: BTreeMap::from([("pRange".into(), json!(range.manifest().range_id))]),
                columns: columns.clone(),
                row_limit: 1,
            })
            .await?;
        if table.columns != columns || table.rows != vec![vec![json!(0)]] {
            return Err(invalid());
        }
        Ok(())
    }
}
fn page_query(schema: &Schema, range: &str, batch: &str, after: usize, until: usize) -> QueryRequest {
    let columns = schema.columns().iter().map(|column| column.name.as_str()).collect::<Vec<_>>().join(", ");
    let text = format!(
        "declare query_parameters(pRange:string,pBatch:string,pAfter:long,pUntil:long);\n{}\n| where RangeId == pRange and BatchId == pBatch\n| where RowOrdinal > pAfter and RowOrdinal <= pUntil\n| project {}\n| distinct {}\n| take {}",
        schema.table(),
        schema.verification_projection(),
        columns,
        until - after + 1
    );
    QueryRequest {
        text,
        parameters: BTreeMap::from([
            ("pRange".into(), json!(range)),
            ("pBatch".into(), json!(batch)),
            ("pAfter".into(), json!(after)),
            ("pUntil".into(), json!(until)),
        ]),
        columns: schema.verification_columns(),
        row_limit: until - after + 1,
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
