//! Store-independent bounded receipt pagination over exact native physical projections.
use super::{
    ProjectionCatalog,
    indexes::{ExpectedRow, IndexKey, ProjectionIndexes},
    sink::AnalyticalStore,
};
use crate::delivery::{recovery::CommittedRange, sink::ReceiptPage};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, MAX_CURSOR_BYTES},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
const PAGE_RESPONSE_BYTES: usize = 12 * 1024 * 1024;
pub struct ReceiptReader {
    store: Arc<dyn AnalyticalStore>,
    catalog: Arc<dyn ProjectionCatalog>,
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
    pub fn new(
        store: Arc<dyn AnalyticalStore>,
        catalog: Arc<dyn ProjectionCatalog>,
        indexes: Arc<ProjectionIndexes>,
    ) -> Self {
        Self { store, catalog, indexes }
    }
    pub async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        if page_rows == 0 || page_rows > 10_000 {
            return Err(DeliveryProblem::Capacity.error());
        }
        if range.manifest().contract.destination() != self.store.destination() {
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
                self.store.audit_membership(range).await?;
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
            let schema = self.catalog.schema(batch.stream)?;
            let key = IndexKey::for_batch(range, &batch.batch_id, schema.digest())?;
            let expected = self.indexes.read(&key).await?;
            let until = page_end(&expected, position.after, page_rows as usize)?;
            let rows = self.store.rows(range, &batch.batch_id, position.after, until).await?;
            if rows.len() > until - position.after {
                return Err(invalid());
            }
            let ordinal_column =
                schema.columns().iter().position(|column| column.name == "RowOrdinal").ok_or_else(invalid)?;
            let mut receipts = Vec::with_capacity(rows.len());
            for row in rows {
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
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_and_row_budgets_both_bound_pages_and_all_rows_remain_reachable() {
        let row = ExpectedRow::new([7; 32], 500_000).unwrap();
        let rows = vec![row; 1000];
        let first = page_end(&rows, 0, 1000).unwrap();
        assert!((1..1000).contains(&first));
        assert!(rows[..first].iter().map(|row| row.response_bytes as usize).sum::<usize>() <= PAGE_RESPONSE_BYTES);
        assert_eq!(page_end(&rows, 0, 3).unwrap(), 3);
        let mut after = 0;
        while after < rows.len() {
            after = page_end(&rows, after, 1000).unwrap();
        }
        assert_eq!(after, 1000);
        assert!(page_end(&[], 0, 1000).is_err());
    }
}
