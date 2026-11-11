//! Unconfirmed native work permanently closes admission for this owner. Only a new owner starts healthy.
use culprit_core::{Result, telemetry::CollectionStop};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Default)]
pub struct ReadFence(Arc<AtomicBool>);
impl ReadFence {
    pub fn ensure_available(&self) -> Result<()> {
        if self.is_uncertain() { Err(CollectionStop::CancellationUnconfirmed.error()) } else { Ok(()) }
    }
    pub fn is_uncertain(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn mark_uncertain(&self) {
        self.0.store(true, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unconfirmed_work_closes_all_clones_without_an_automatic_reset() {
        let fence = ReadFence::default();
        let other = fence.clone();
        assert!(fence.ensure_available().is_ok());
        other.mark_uncertain();
        assert!(fence.ensure_available().is_err());
        assert!(other.is_uncertain());
    }
}
