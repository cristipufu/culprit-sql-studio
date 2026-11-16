use super::{EvidenceRequest, EvidenceService};
use culprit_collector::{
    analytical::evidence::{EvidenceCapture, EvidenceFamily, EvidenceReadBudget, EvidenceReadMetrics},
    capture::CollectionCancellation,
    delivery::{object_store::ObjectRecoveryStore, recovery::reopen},
};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, DeliveryProblem},
};
use std::time::Duration;
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
        // Any failed or cancelled read may have consumed work without returning metrics. Do not reuse its allowance.
        let mut budget = self.remaining.take().ok_or_else(capacity)?;
        budget.page_rows = budget.page_rows.min(budget.max_rows);
        budget.page_bytes = u64::from(budget.page_bytes).min(budget.max_bytes) as u32;
        let request = EvidenceRequest { range_id: range_id.into(), families: families.to_vec(), budget };
        request.validate()?;
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        let reading = async {
            let recovery =
                ObjectRecoveryStore::open(self.service.recovery.clone(), self.service.contract.clone()).await?;
            let range = reopen(&recovery, &request.range_id).await?;
            self.service.reader.read_capture(&range, &request.families, budget, Some(&recovery)).await
        };
        let capture = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(CollectionStop::Cancelled.error()),
            result = tokio::time::timeout_at(self.deadline, reading) => {
                result.map_err(|_| CollectionStop::TimedOut.error())??
            }
        };
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        self.remaining = Some(deduct(budget, capture.metrics())?);
        Ok(capture)
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
