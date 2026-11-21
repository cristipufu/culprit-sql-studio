//! One allowance for the existing parallel sessions of a normal PostgreSQL operation.
use super::*;
use std::sync::{Arc, Mutex};

pub(crate) const APPLICATION_QUERY_LIMIT: u32 = 512;
pub(super) const APPLICATION_DURATION: Duration = Duration::from_secs(60);
#[derive(Clone)]
pub(crate) struct ApplicationReadScope {
    pub(super) deadline: Instant,
    state: Arc<Mutex<Consumption>>,
}
#[derive(Default)]
struct Consumption {
    queries: u32,
    stopped: Option<CollectionStop>,
}
impl ApplicationReadScope {
    pub(crate) fn new() -> Self {
        Self::until(Instant::now() + APPLICATION_DURATION)
    }
    pub(crate) fn until(deadline: Instant) -> Self {
        Self { deadline: deadline.min(Instant::now() + APPLICATION_DURATION), state: Arc::default() }
    }
    fn available(&self, state: &mut Consumption) -> Result<()> {
        let stop = state.stopped.or_else(|| {
            if Instant::now() >= self.deadline {
                Some(CollectionStop::TimedOut)
            } else if state.queries >= APPLICATION_QUERY_LIMIT {
                Some(CollectionStop::QueryLimit)
            } else {
                None
            }
        });
        if let Some(stop) = stop {
            state.stopped = Some(stop);
            return Err(stop.error());
        }
        Ok(())
    }
    pub(super) fn ensure_available(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        self.available(&mut state)
    }
    pub(super) fn start(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        self.available(&mut state)?;
        state.queries += 1;
        Ok(())
    }
    pub(crate) fn finish(&self) -> Result<()> {
        let state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        if let Some(stop) = state.stopped {
            return Err(stop.error());
        }
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
