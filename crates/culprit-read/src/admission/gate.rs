//! One native-work permit is shared by report scans and telemetry captures.
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use tokio::sync::{Semaphore, SemaphorePermit};

pub struct SourceGate {
    semaphore: Semaphore,
}
impl Default for SourceGate {
    fn default() -> Self {
        Self { semaphore: Semaphore::new(1) }
    }
}
impl SourceGate {
    pub async fn acquire(&self) -> Result<SourcePermit<'_>> {
        let permit = self.semaphore.acquire().await.map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        Ok(SourcePermit { gate: self, _permit: permit, state: State::Ready })
    }
    pub fn is_closed(&self) -> bool {
        self.semaphore.is_closed()
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    InFlight,
    Settled,
}
#[must_use]
pub struct SourcePermit<'a> {
    gate: &'a SourceGate,
    _permit: SemaphorePermit<'a>,
    state: State,
}
impl SourcePermit<'_> {
    /// Call before native work may start; cancellation after this point retains the uncertainty.
    pub fn begin(&mut self) -> Result<()> {
        if self.state != State::Ready {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.state = State::InFlight;
        Ok(())
    }
    pub fn settle(&mut self, shutdown: SourceShutdown) -> Result<()> {
        if self.state != State::InFlight {
            return Err(CollectionStop::InvalidPage.error());
        }
        if shutdown == SourceShutdown::Unconfirmed {
            self.gate.semaphore.close();
        }
        self.state = State::Settled;
        Ok(())
    }
}
impl Drop for SourcePermit<'_> {
    fn drop(&mut self) {
        if self.state == State::InFlight {
            self.gate.semaphore.close();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unused_and_confirmed_permits_release_admission_but_uncertain_or_dropped_work_closes_it() {
        let gate = SourceGate::default();
        drop(gate.acquire().await.unwrap());
        let mut permit = gate.acquire().await.unwrap();
        permit.begin().unwrap();
        permit.settle(SourceShutdown::Quiescent).unwrap();
        drop(permit);
        let mut permit = gate.acquire().await.unwrap();
        permit.begin().unwrap();
        permit.settle(SourceShutdown::Unconfirmed).unwrap();
        drop(permit);
        assert!(gate.acquire().await.is_err());
        let gate = SourceGate::default();
        let mut permit = gate.acquire().await.unwrap();
        permit.begin().unwrap();
        drop(permit);
        assert!(gate.is_closed());
        assert!(gate.acquire().await.is_err());
    }
    #[tokio::test]
    async fn duplicate_state_transitions_never_authorize_more_work() {
        let gate = SourceGate::default();
        let mut permit = gate.acquire().await.unwrap();
        assert!(permit.settle(SourceShutdown::Quiescent).is_err());
        permit.begin().unwrap();
        assert!(permit.begin().is_err());
        drop(permit);
        assert!(gate.is_closed());
    }
}
