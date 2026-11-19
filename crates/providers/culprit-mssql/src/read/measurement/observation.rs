//! One bounded sample loop observes the same native future without replacing its result or ownership.
use super::*;
use std::future::Future;

pub(super) trait SampleSource: Send {
    fn sample(&mut self) -> impl Future<Output = Result<Sample>> + Send;
}
impl SampleSource for Observer {
    async fn sample(&mut self) -> Result<Sample> {
        Observer::sample(self).await
    }
}

pub(super) async fn settled_sample(owner: &mut ReadSession, observer: &mut impl SampleSource) -> Result<Sample> {
    // The original task is joined while its client remains retained. A quiet independent sample grants no
    // acknowledgement or reconnect authority; close still owns the source fence and physical release.
    owner.settle_native().await;
    observer.sample().await
}

pub(super) async fn await_observed<T>(
    work: impl Future<Output = ReadResult<T>>,
    observer: &mut impl SampleSource,
    phase: &mut Phase,
    active_seen: &AtomicBool,
    template_watch: Option<&TemplateObservation>,
) -> ReadResult<T> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = tokio::time::sleep(SAMPLE_INTERVAL), if phase.observer_error.is_none() => {
                match observer.sample().await {
                    Ok(sample) => {
                        if sample.active { active_seen.store(true, Ordering::Release); }
                        if let Some(watch) = template_watch { watch.observe(&phase.sql, sample.active); }
                        phase.observe(&sample);
                    }
                    // The same SQL can span stream start and several batches. Its first observation loss
                    // remains sticky; await the same native result and never retry sampling in that phase.
                    Err(error) => phase.observer_error = Some(error),
                }
            }
        }
    }
}

pub(super) fn measured_result<T>(
    native: ReadResult<T>,
    accounting: Result<()>,
    shutdown: Option<SourceShutdown>,
) -> ReadResult<T> {
    if shutdown == Some(SourceShutdown::Unconfirmed) {
        return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
    }
    let value = native?;
    accounting.map_err(ReadError::Source)?;
    Ok(value)
}

#[cfg(test)]
mod tests;
