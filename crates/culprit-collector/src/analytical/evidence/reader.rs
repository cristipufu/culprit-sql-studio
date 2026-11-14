use super::{
    decode::{self, Decoded},
    *,
};
use crate::{
    analytical::{
        ProjectionCatalog,
        indexes::{ExpectedRow, IndexKey, ProjectionIndexes},
        publication,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
    },
    delivery::{evidence::BatchManifest, recovery::CommittedRange},
};
use culprit_core::{Result, telemetry::*};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc, time::Duration};

pub struct EvidenceReader {
    store: Arc<dyn AnalyticalReadStore>,
    indexes: Arc<ProjectionIndexes>,
}
impl EvidenceReader {
    pub fn new(store: Arc<dyn AnalyticalReadStore>, indexes: Arc<ProjectionIndexes>) -> Self {
        Self { store, indexes }
    }

    pub async fn read_capture(
        &self,
        range: &CommittedRange,
        families: &[EvidenceFamily],
        budget: EvidenceReadBudget,
    ) -> Result<EvidenceCapture> {
        budget.validate()?;
        range.manifest().validate()?;
        if families.is_empty()
            || families.len() > EvidenceFamily::ALL.len()
            || families.iter().enumerate().any(|(index, family)| families[..index].contains(family))
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let catalog = self.store.catalog();
        if range.manifest().contract.destination() != self.store.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if range.manifest().contract.schema_digest() != catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let rows = range
            .manifest()
            .batches
            .iter()
            .filter(|batch| families.iter().any(|family| family.stream() == batch.stream))
            .try_fold(0usize, |count, batch| count.checked_add(batch.rows.len()).ok_or_else(capacity))?;
        if rows > budget.max_rows as usize {
            return Err(capacity());
        }
        tokio::time::timeout(
            Duration::from_millis(u64::from(budget.timeout_ms)),
            self.read(range, families, budget, catalog),
        )
        .await
        .map_err(|_| CollectionStop::TimedOut.error())?
    }

    async fn read(
        &self,
        range: &CommittedRange,
        families: &[EvidenceFamily],
        budget: EvidenceReadBudget,
        catalog: Arc<dyn ProjectionCatalog>,
    ) -> Result<EvidenceCapture> {
        let manifest = range.manifest();
        let mut capture = EvidenceCapture {
            range_id: manifest.range_id.clone(),
            recovery_manifest_sha256: range.manifest_digest().into(),
            capture_manifest_sha256: manifest.capture_manifest_sha256.clone(),
            source: manifest.source.clone(),
            capture: manifest.capture.clone(),
            collected_from: manifest.collected_from.clone(),
            collected_until: manifest.collected_until.clone(),
            families: families.to_vec(),
            records: vec![],
            gaps: vec![],
            metrics: EvidenceReadMetrics::default(),
        };
        let mut state = ReadState { budget, metrics: &mut capture.metrics };
        state.query()?;
        let publications = self.store.publication_rows(range, state.byte_limit()?).await?;
        state.response(&publications)?;
        if publications.rows.is_empty() {
            capture.gaps.push(EvidenceGap {
                stream: None,
                batch_id: None,
                row_ordinal: None,
                reason: EvidenceGapReason::NotPublished,
            });
            return Ok(capture);
        }
        if publications.rows.len() > 2 {
            return Err(invalid());
        }
        let schema = catalog.publication();
        let wanted = super::super::projection::digest(&schema.project(&publication::input(catalog.as_ref(), range)?)?)?;
        for row in publications.rows {
            if schema.returned_digest(&row)? != wanted {
                return Err(invalid());
            }
        }
        state.query()?;
        let audit_bytes = self.store.audit_membership(range, state.byte_limit()?).await?;
        state.wire_response(audit_bytes)?;
        for family in families {
            let batches = manifest
                .batches
                .iter()
                .filter(|batch| batch.delivery_required && batch.stream == family.stream())
                .collect::<Vec<_>>();
            if batches.is_empty() {
                capture.gaps.push(EvidenceGap {
                    stream: Some(family.stream()),
                    batch_id: None,
                    row_ordinal: None,
                    reason: EvidenceGapReason::FamilyNotCaptured,
                });
            }
            for batch in batches {
                self.batch(range, batch, catalog.as_ref(), &mut state, &mut capture.records, &mut capture.gaps).await?;
            }
        }
        capture
            .records
            .sort_by(|left, right| (&left.batch_id, left.row_ordinal).cmp(&(&right.batch_id, right.row_ordinal)));
        Ok(capture)
    }

    async fn batch(
        &self,
        range: &CommittedRange,
        batch: &BatchManifest,
        catalog: &dyn ProjectionCatalog,
        state: &mut ReadState<'_>,
        records: &mut Vec<VerifiedEvidenceRecord>,
        gaps: &mut Vec<EvidenceGap>,
    ) -> Result<()> {
        if batch.rows.is_empty() {
            return Ok(());
        }
        let schema = catalog.schema(batch.stream)?;
        let key = IndexKey::for_batch(range, &batch.batch_id, schema.digest())?;
        state.proof(key.encoded_bytes())?;
        let expected = match self.indexes.read(&key).await {
            Ok(expected) => expected,
            Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) => {
                gaps.push(gap(batch, None, EvidenceGapReason::ProjectionProofUnavailable));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let ordinal_column =
            schema.columns().iter().position(|column| column.name == "RowOrdinal").ok_or_else(invalid)?;
        let mut after = 0;
        while after < expected.len() {
            let until = state.page_end(&expected, after)?;
            state.query()?;
            state.metrics.pages += 1;
            let page = self.store.rows(range, &batch.batch_id, after, until, state.byte_limit()?).await?;
            if page.rows.len() > until - after + 1 {
                return Err(invalid());
            }
            state.response(&page)?;
            state.metrics.returned_rows =
                state.metrics.returned_rows.checked_add(page.rows.len() as u32).ok_or_else(capacity)?;
            let mut seen = BTreeSet::new();
            for row in page.rows {
                let ordinal = row
                    .get(ordinal_column)
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|ordinal| *ordinal > after && *ordinal <= until)
                    .ok_or_else(invalid)?;
                if schema.returned_digest(&row)? != expected[ordinal - 1].digest {
                    return Err(invalid());
                }
                if !seen.insert(ordinal) {
                    continue;
                }
                let values = self.store.logical_values(batch.stream, &row)?;
                match decode::record(range, batch.stream, &values, &batch.rows[ordinal - 1])? {
                    Decoded::Record(row) => records.push(VerifiedEvidenceRecord {
                        batch_id: batch.batch_id.clone(),
                        row_ordinal: ordinal as u32,
                        native_payload_sha256: batch.rows[ordinal - 1].payload_digest.clone(),
                        row: *row,
                    }),
                    Decoded::Unavailable(reason) => gaps.push(gap(batch, Some(ordinal), reason)),
                }
            }
            for ordinal in after + 1..=until {
                if !seen.contains(&ordinal) {
                    gaps.push(gap(batch, Some(ordinal), EvidenceGapReason::MissingRows));
                }
            }
            after = until;
        }
        Ok(())
    }
}

struct ReadState<'a> {
    budget: EvidenceReadBudget,
    metrics: &'a mut EvidenceReadMetrics,
}
impl ReadState<'_> {
    fn query(&mut self) -> Result<()> {
        self.metrics.queries = self
            .metrics
            .queries
            .checked_add(1)
            .filter(|count| *count <= self.budget.max_queries)
            .ok_or_else(capacity)?;
        Ok(())
    }
    fn byte_limit(&self) -> Result<usize> {
        self.budget
            .max_bytes
            .checked_sub(self.metrics.response_bytes)
            .map(|remaining| remaining.min(u64::from(self.budget.page_bytes)) as usize)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(capacity)
    }
    fn response(&mut self, page: &AnalyticalReadPage) -> Result<()> {
        super::response_bytes::count(&page.rows, self.byte_limit()?)?;
        self.wire_response(page.response_bytes)
    }
    fn wire_response(&mut self, bytes: usize) -> Result<()> {
        if bytes == 0 {
            return Err(invalid());
        }
        if bytes > self.byte_limit()? {
            return Err(capacity());
        }
        self.metrics.response_bytes = self.metrics.response_bytes.checked_add(bytes as u64).ok_or_else(capacity)?;
        Ok(())
    }
    fn proof(&mut self, bytes: usize) -> Result<()> {
        self.metrics.proof_bytes = self
            .metrics
            .proof_bytes
            .checked_add(bytes as u64)
            .filter(|count| *count <= self.budget.max_proof_bytes)
            .ok_or_else(capacity)?;
        Ok(())
    }
    fn page_end(&self, expected: &[ExpectedRow], after: usize) -> Result<usize> {
        let mut bytes = 0u64;
        let mut until = after;
        for row in expected.iter().skip(after).take(self.budget.page_rows as usize) {
            if bytes + u64::from(row.response_bytes) > self.byte_limit()? as u64 {
                break;
            }
            bytes += u64::from(row.response_bytes);
            until += 1;
        }
        if until == after {
            return Err(capacity());
        }
        Ok(until)
    }
}
fn gap(batch: &BatchManifest, ordinal: Option<usize>, reason: EvidenceGapReason) -> EvidenceGap {
    EvidenceGap {
        stream: Some(batch.stream),
        batch_id: Some(batch.batch_id.clone()),
        row_ordinal: ordinal.map(|value| value as u32),
        reason,
    }
}
fn capacity() -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
