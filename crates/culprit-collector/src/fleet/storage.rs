//! Capture ownership and exact committed-range receipts, independent of provider connections.
use crate::{delivery::recovery::RecoveryStore, outbox::Outbox};
use culprit_core::{
    Result,
    telemetry::{CapturedRange, DeliveryProblem},
};

pub(super) struct CaptureStorage<'a> {
    pub outbox: &'a mut Outbox,
    pub recovery: Option<&'a dyn RecoveryStore>,
}
impl<'a> CaptureStorage<'a> {
    pub fn local(outbox: &'a mut Outbox) -> Self {
        Self { outbox, recovery: None }
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(expected) = self.outbox.recovery_store_id()
            && self.recovery.is_none_or(|store| store.id() != expected)
        {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(())
    }
    pub async fn captured(&self, range: &CapturedRange) -> Result<bool> {
        if self.outbox.captured_range(range).await?.is_some() {
            return Ok(true);
        }
        match self.recovery {
            Some(store) => Ok(store.captured_range(range).await?.is_some()),
            None => Ok(false),
        }
    }
}
