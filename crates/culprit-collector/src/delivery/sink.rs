//! Transport acknowledgement is a hint; publication requires exact distinct row membership and valid projections.
pub use super::verification::{VerifiedRange, verify};
use super::{contract::*, evidence::*, recovery::CommittedRange};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAttempt {
    pub id: String,
    pub ordinal: u32,
}
impl DeliveryAttempt {
    pub fn new(ordinal: u32) -> Result<Self> {
        if ordinal == 0 {
            return Err(DeliveryProblem::NotReady.error());
        }
        Ok(Self { id: crate::durable_fs::random_id()?, ordinal })
    }
}
#[derive(Clone, Debug)]
pub enum Submission {
    Accepted,
    Unknown,
    Rejected,
}
#[derive(Clone, Debug)]
pub struct ReceiptPage {
    pub rows: Vec<RowReceipt>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Publication {
    pub range_id: String,
    pub manifest_digest: String,
}
/// A destination has read back the exact committed completion record. This is a recovery capability,
/// not a guarantee about future retention or mutations at the destination.
pub struct PublishedRange {
    range: CommittedRange,
}
impl PublishedRange {
    pub fn committed(&self) -> &CommittedRange {
        &self.range
    }
}
pub async fn read_publication(sink: &dyn TelemetrySink, range: &CommittedRange) -> Result<PublishedRange> {
    let publication =
        Publication { range_id: range.manifest().range_id.clone(), manifest_digest: range.manifest_digest().into() };
    if !sink.publication_visible(&publication, range).await? {
        return Err(DeliveryProblem::NotReady.error());
    }
    Ok(PublishedRange { range: range.clone() })
}
#[derive(Clone, Debug)]
pub enum DeliveryUpdate {
    Retry { range_id: String },
    Quarantine { range_id: String },
    Reconcile { range_id: String },
}
pub struct DeliveryUpdates {
    pub updates: Vec<DeliveryUpdate>,
    pub next_cursor: String,
    pub expired: bool,
}
#[async_trait]
pub trait TelemetrySink: Send + Sync {
    async fn validate_contract(&self, contract: &Contract) -> Result<()>;
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission>;
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage>;
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication>;
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool>;
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates>;
}
/// Explicit whole-range convenience. Background workers checkpoint each batch between bounded operations.
pub async fn dispatch_range(
    sink: &dyn TelemetrySink,
    range: &CommittedRange,
    attempt: &DeliveryAttempt,
) -> Result<Submission> {
    let mut unknown = false;
    for batch in range.manifest().batches.iter().filter(|batch| batch.delivery_required) {
        match sink.dispatch(range, attempt, &batch.batch_id).await? {
            Submission::Rejected => return Ok(Submission::Rejected),
            Submission::Unknown => unknown = true,
            Submission::Accepted => (),
        }
    }
    Ok(if unknown { Submission::Unknown } else { Submission::Accepted })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationBudget {
    pub page_rows: u32,
    pub max_pages: u32,
    pub max_physical_rows: u32,
}
impl Default for VerificationBudget {
    fn default() -> Self {
        // Large native ranges may need smaller pages to stay within a destination's byte budget.
        Self { page_rows: 1000, max_pages: 2048, max_physical_rows: 200_000 }
    }
}
