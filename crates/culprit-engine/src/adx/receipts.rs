//! Exact typed readback, with ordinal and byte bounded pages. No count-only mapping proof or source-blob rereads.
use super::{
    catalog::SchemaCatalog,
    client::{QueryRequest, QueryTransport},
    indexes::{ExpectedRow, IndexKey, ProjectionIndexes},
    response::Column,
    schema::Schema,
};
use culprit_collector::delivery::{recovery::CommittedRange, sink::ReceiptPage};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, MAX_CURSOR_BYTES},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

#[cfg(test)]
mod tests;
const PAGE_RESPONSE_BYTES: usize = 12 * 1024 * 1024;

pub struct ReceiptReader {
    query: Arc<dyn QueryTransport>,
    catalog: Arc<SchemaCatalog>,
    indexes: Arc<ProjectionIndexes>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    manifest: String,
    batch: usize,
    after: usize,
}
impl ReceiptReader {
    pub fn new(query: Arc<dyn QueryTransport>, catalog: Arc<SchemaCatalog>, indexes: Arc<ProjectionIndexes>) -> Self {
        Self { query, catalog, indexes }
    }
    pub async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        if page_rows == 0 || page_rows > 10_000 {
            return Err(DeliveryProblem::Capacity.error());
        }
        if range.manifest().contract.destination() != self.query.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if range.manifest().contract.schema_digest() != self.catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let mut position = match cursor {
            Some(text) if text.len() <= MAX_CURSOR_BYTES => {
                serde_json::from_str::<Cursor>(text).map_err(|_| invalid())?
            }
            Some(_) => return Err(invalid()),
            None => {
                self.audit_membership(range).await?;
                Cursor { manifest: range.manifest_digest().into(), batch: 0, after: 0 }
            }
        };
        if position.manifest != range.manifest_digest()
            || position.batch >= range.manifest().batches.len() && cursor.is_some()
        {
            return Err(invalid());
        }
        while let Some(batch) = range.manifest().batches.get(position.batch) {
            if !batch.delivery_required || batch.rows.is_empty() {
                if position.after != 0 {
                    return Err(invalid());
                }
                position.batch += 1;
                continue;
            }
            if position.after >= batch.rows.len() {
                return Err(invalid());
            }
            let schema = self.catalog.get(batch.stream)?;
            let key = IndexKey::for_batch(range, &batch.batch_id, schema.digest())?;
            let expected = self.indexes.read(&key).await?;
            let until = page_end(&expected, position.after, page_rows as usize)?;
            let request = page_query(schema, &range.manifest().range_id, &batch.batch_id, position.after, until);
            let table = self.query.query(request).await?;
            if table.columns != schema.verification_columns() || table.rows.len() > until - position.after {
                return Err(invalid());
            }
            let ordinal_column =
                schema.columns().iter().position(|column| column.name == "RowOrdinal").ok_or_else(invalid)?;
            let mut receipts = Vec::with_capacity(table.rows.len());
            for row in table.rows {
                let ordinal = row
                    .get(ordinal_column)
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|ordinal| *ordinal > position.after && *ordinal <= until)
                    .ok_or_else(invalid)?;
                if schema.returned_digest(&row)? != expected[ordinal - 1].digest {
                    return Err(invalid());
                }
                receipts.push(batch.rows[ordinal - 1].clone());
            }
            if until == batch.rows.len() {
                position.batch += 1;
                position.after = 0;
            } else {
                position.after = until;
            }
            // Skip only explicitly empty or recovery-only streams; empty successful ranges still passed the audit.
            while range
                .manifest()
                .batches
                .get(position.batch)
                .is_some_and(|batch| !batch.delivery_required || batch.rows.is_empty())
            {
                position.batch += 1;
            }
            let next_cursor = if position.batch < range.manifest().batches.len() {
                Some(serde_json::to_string(&position).map_err(|_| invalid())?)
            } else {
                None
            };
            return Ok(ReceiptPage { rows: receipts, next_cursor, complete: true });
        }
        Ok(ReceiptPage { rows: vec![], next_cursor: None, complete: true })
    }
    async fn audit_membership(&self, range: &CommittedRange) -> Result<()> {
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
fn page_end(expected: &[ExpectedRow], after: usize, limit: usize) -> Result<usize> {
    let mut end = after;
    let mut bytes = 0usize;
    for row in expected.iter().skip(after).take(limit) {
        if row.response_bytes as usize > PAGE_RESPONSE_BYTES.saturating_sub(bytes) {
            break;
        }
        bytes += row.response_bytes as usize;
        end += 1;
    }
    if end == after {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok(end)
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
