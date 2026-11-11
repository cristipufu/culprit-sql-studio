//! Unconfirmed native work permanently closes admission for this owner. Only a new owner starts healthy.
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Default)]
pub struct ReadFence(Arc<AtomicBool>);

/// An owned worker must explicitly establish shutdown. Panic, abortion and abandoned setup all retain uncertainty.
pub struct ReadWork {
    fence: ReadFence,
    confirmed: bool,
}
impl ReadWork {
    pub fn complete(mut self, shutdown: SourceShutdown) {
        self.confirmed = shutdown == SourceShutdown::Quiescent;
    }
}
impl Drop for ReadWork {
    fn drop(&mut self) {
        if !self.confirmed {
            self.fence.mark_uncertain();
        }
    }
}
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
    pub fn start_work(&self) -> Result<ReadWork> {
        self.ensure_available()?;
        Ok(ReadWork { fence: self.clone(), confirmed: false })
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
    #[test]
    fn abandoned_or_unconfirmed_workers_close_admission() {
        for shutdown in [None, Some(SourceShutdown::Unconfirmed)] {
            let fence = ReadFence::default();
            let work = fence.start_work().unwrap();
            if let Some(shutdown) = shutdown {
                work.complete(shutdown);
            } else {
                drop(work);
            }
            assert!(fence.is_uncertain());
            assert!(fence.start_work().is_err());
        }
    }
    #[test]
    fn a_confirmed_worker_never_clears_another_workers_uncertainty() {
        let fence = ReadFence::default();
        let first = fence.start_work().unwrap();
        let second = fence.start_work().unwrap();
        drop(first);
        second.complete(SourceShutdown::Quiescent);
        assert!(fence.is_uncertain());
    }
    #[test]
    fn confirmed_workers_leave_admission_available() {
        let fence = ReadFence::default();
        fence.start_work().unwrap().complete(SourceShutdown::Quiescent);
        assert!(fence.ensure_available().is_ok());
    }
}
