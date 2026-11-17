use super::{EvidenceRequest, EvidenceService};
use culprit_collector::{
    analytical::evidence::{
        EvidenceCapture, EvidenceCaptureMetadata, EvidenceFamily, EvidenceReadBudget, EvidenceReadMetrics,
    },
    capture::CollectionCancellation,
    delivery::{
        object_store::ObjectRecoveryStore,
        recovery::{CommittedRange, reopen},
    },
};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, DeliveryProblem, valid_digest},
};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

/// One caller action owns the cumulative warehouse/recovery allowance and an absolute deadline.
/// Transports retain their own lifecycle limits; dropping a read does not prove remote cancellation.
pub struct EvidenceSession<'a> {
    service: &'a EvidenceService,
    cancellation: &'a CollectionCancellation,
    deadline: Instant,
    remaining: Option<EvidenceReadBudget>,
}

impl<'a> EvidenceSession<'a> {
    pub(super) fn new(
        service: &'a EvidenceService,
        budget: EvidenceReadBudget,
        cancellation: &'a CollectionCancellation,
    ) -> Result<Self> {
        budget.validate()?;
        Ok(Self {
            service,
            cancellation,
            deadline: Instant::now() + Duration::from_millis(u64::from(budget.timeout_ms)),
            remaining: Some(budget),
        })
    }

    pub async fn read(&mut self, range_id: &str, families: &[EvidenceFamily]) -> Result<EvidenceCapture> {
        self.read_with(
            range_id,
            |budget| EvidenceRequest { range_id: range_id.into(), families: families.to_vec(), budget }.validate(),
            |service, range, recovery, budget| async move {
                let capture = service.reader.read_capture(&range, families, budget, Some(&recovery)).await?;
                let metrics = capture.metrics().clone();
                Ok((capture, metrics))
            },
        )
        .await
    }

    /// Reads only original capture metadata and its publication proof using this same action allowance.
    pub async fn read_metadata(&mut self, range_id: &str) -> Result<EvidenceCaptureMetadata> {
        self.read_with(
            range_id,
            |_| Ok(()),
            |service, range, _recovery, budget| async move {
                let metadata = service.reader.read_metadata(&range, budget).await?;
                let metrics = metadata.metrics().clone();
                Ok((metadata, metrics))
            },
        )
        .await
    }

    async fn read_with<T, F, Reading>(
        &mut self,
        range_id: &str,
        validate: impl FnOnce(EvidenceReadBudget) -> Result<()>,
        read: F,
    ) -> Result<T>
    where
        F: FnOnce(&'a EvidenceService, CommittedRange, ObjectRecoveryStore, EvidenceReadBudget) -> Reading,
        Reading: Future<Output = Result<(T, EvidenceReadMetrics)>>,
    {
        // Any failed or cancelled read may have consumed work without returning metrics. Do not reuse its allowance.
        let mut budget = self.remaining.take().ok_or_else(capacity)?;
        budget.page_rows = budget.page_rows.min(budget.max_rows);
        budget.page_bytes = u64::from(budget.page_bytes).min(budget.max_bytes) as u32;
        budget.validate()?;
        if !valid_digest(range_id) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        validate(budget)?;
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        let reading = async {
            let recovery =
                ObjectRecoveryStore::open(self.service.recovery.clone(), self.service.contract.clone()).await?;
            let range = reopen(&recovery, range_id).await?;
            read(self.service, range, recovery, budget).await
        };
        let (result, metrics) = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(CollectionStop::Cancelled.error()),
            result = tokio::time::timeout_at(self.deadline, reading) => {
                result.map_err(|_| CollectionStop::TimedOut.error())??
            }
        };
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        self.remaining = Some(deduct(budget, &metrics)?);
        Ok(result)
    }
}

fn deduct(mut budget: EvidenceReadBudget, used: &EvidenceReadMetrics) -> Result<EvidenceReadBudget> {
    budget.max_rows = budget.max_rows.checked_sub(used.selected_rows).ok_or_else(capacity)?;
    budget.max_queries = budget.max_queries.checked_sub(used.queries).ok_or_else(capacity)?;
    budget.max_bytes = budget.max_bytes.checked_sub(used.response_bytes).ok_or_else(capacity)?;
    budget.max_proof_bytes = budget.max_proof_bytes.checked_sub(used.proof_bytes).ok_or_else(capacity)?;
    if let Some(recovery) = &mut budget.recovery_payloads {
        recovery.max_objects = recovery.max_objects.checked_sub(used.recovery_objects).ok_or_else(capacity)?;
        recovery.max_bytes = recovery.max_bytes.checked_sub(used.recovery_bytes).ok_or_else(capacity)?;
    }
    Ok(budget)
}

fn capacity() -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}
