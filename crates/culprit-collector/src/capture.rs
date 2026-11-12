//! One bounded observation. Source ownership, cancellation and file completion have one controller.
mod writer;
use crate::outbox::CaptureLease;
use culprit_core::{
    Error, Result,
    telemetry::{
        CaptureIdentity, CollectionManifest, CollectionRead, CollectionRequest, CollectionStatus, CollectionStop,
        SourceShutdown, TelemetrySource, TelemetryStream,
    },
};
use culprit_read::admission::SourceGate;
use std::{collections::HashSet, path::Path, time::Duration};
use tokio::{sync::watch, time::Instant};
use writer::ObservationWriter;

const SOURCE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct CollectionCancellation {
    sender: watch::Sender<bool>,
}
impl Default for CollectionCancellation {
    fn default() -> Self {
        Self { sender: watch::channel(false).0 }
    }
}
impl CollectionCancellation {
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

#[async_trait::async_trait]
pub trait SourceFactory: Send + Sync {
    /// Authenticate and configure a lazy source. Native database work belongs to describe/read/verify, where the
    /// controller owns admission and durable cancellation state; opening a factory must not issue database queries.
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>>;
}

pub async fn collect_once(
    admission: &SourceGate,
    factory: &dyn SourceFactory,
    request: CollectionRequest,
    output: &Path,
    cancellation: &CollectionCancellation,
) -> Result<CollectionManifest> {
    let capture = culprit_read::admission::observation_identity()?;
    collect(admission, factory, request, output, capture, cancellation, None).await
}

pub async fn collect(
    admission: &SourceGate,
    factory: &dyn SourceFactory,
    request: CollectionRequest,
    output: &Path,
    capture: CaptureIdentity,
    cancellation: &CollectionCancellation,
    lease: Option<&mut CaptureLease<'_>>,
) -> Result<CollectionManifest> {
    request.validate()?;
    let files = ObservationWriter::create(output.to_owned(), request.clone(), capture).await?;
    collect_prepared(admission, factory, request, files, cancellation, lease).await
}

async fn collect_prepared(
    admission: &SourceGate,
    factory: &dyn SourceFactory,
    request: CollectionRequest,
    files: ObservationWriter,
    cancellation: &CollectionCancellation,
    mut lease: Option<&mut CaptureLease<'_>>,
) -> Result<CollectionManifest> {
    let deadline = Instant::now() + Duration::from_millis(request.budget.total_timeout_ms.into());
    let permit = tokio::select! {
        biased;
        _=cancellation.cancelled()=>return files.finish(0,None,Some(CollectionStop::Cancelled)).await,
        _=tokio::time::sleep_until(deadline)=>return files.finish(0,None,Some(CollectionStop::TimedOut)).await,
        permit=admission.acquire()=>match permit {
            Ok(permit)=>permit,
            Err(_)=>{ files.finish(0,None,Some(CollectionStop::CancellationUnconfirmed)).await?; return Err(CollectionStop::CancellationUnconfirmed.error()); }
        },
    };
    let mut admission = permit;
    let provider = tokio::select! {
        biased;
        _=cancellation.cancelled()=>return files.finish(0,None,Some(CollectionStop::Cancelled)).await,
        _=tokio::time::sleep_until(deadline)=>return files.finish(0,None,Some(CollectionStop::TimedOut)).await,
        provider=factory.open(&request)=>provider,
    };
    let mut source = match provider {
        Ok(source) => source,
        Err(error) => {
            files.finish(0, None, Some(CollectionStop::from_error(&error))).await?;
            return Err(error);
        }
    };
    if let Some(lease) = &mut lease {
        lease.source_started().await?;
    }
    admission.begin()?;
    let result = tokio::select! {
        biased;
        _=cancellation.cancelled()=>Err(CollectionStop::Cancelled.error()),
        _=tokio::time::sleep_until(deadline)=>Err(CollectionStop::TimedOut.error()),
        result=observe(source.as_mut(),&files,&request,lease.as_deref())=>result,
    };
    let mut shutdown =
        tokio::time::timeout(SOURCE_SHUTDOWN_TIMEOUT, source.close()).await.unwrap_or(SourceShutdown::Unconfirmed);
    let metrics = source.read_metrics();
    if metrics.as_ref().is_some_and(|metrics| metrics.has_unconfirmed_work()) {
        shutdown = SourceShutdown::Unconfirmed;
    }
    admission.settle(shutdown)?;
    if let Some(lease) = &mut lease {
        lease.source_closed(shutdown).await?;
    }
    let queries = source.query_count();
    // Native shutdown precedes waiting for any outstanding storage operation. A slow disk must not delay Attention.
    if shutdown == SourceShutdown::Unconfirmed {
        return files.finish(queries, metrics, Some(CollectionStop::CancellationUnconfirmed)).await;
    }
    match result {
        Ok(failure) => {
            let manifest = files.finish(queries, metrics, None).await?;
            if let Some(error) = failure { Err(error) } else { Ok(manifest) }
        }
        Err(error) => {
            let manifest = files.finish(queries, metrics, Some(CollectionStop::from_error(&error))).await?;
            if matches!(error.code, culprit_core::ErrorCode::Timeout | culprit_core::ErrorCode::QueryFailed)
                && matches!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(_)))
            {
                Ok(manifest)
            } else {
                Err(error)
            }
        }
    }
}

async fn observe(
    source: &mut dyn TelemetrySource,
    files: &ObservationWriter,
    request: &CollectionRequest,
    lease: Option<&CaptureLease<'_>>,
) -> Result<Option<Error>> {
    let description = source.describe().await?;
    let baseline = if request.collection() == culprit_core::telemetry::TelemetryCollection::Activity {
        match lease {
            Some(lease) => lease.baseline(&description).await?,
            None => None,
        }
    } else {
        None
    };
    files.apply(move |files| files.describe(description)).await?;
    let mut failure = None;
    for (index, stream) in request.collection().source_streams().iter().copied().enumerate() {
        match collect_stream(source, files, index, stream).await {
            Ok(Some(reason)) => {
                files
                    .apply(move |files| {
                        files.stop(index, reason);
                        Ok(())
                    })
                    .await?
            }
            Ok(None) => files.apply(move |files| files.seal(index)).await?,
            Err(error) if CollectionStop::from_error(&error) == CollectionStop::StorageFailed => return Err(error),
            Err(error) => {
                let recorded = error.clone();
                files
                    .apply(move |files| {
                        files.fail_stream(index, &recorded);
                        Ok(())
                    })
                    .await?;
                failure.get_or_insert(error);
            }
        }
    }
    let complete = files
        .apply(|files| {
            Ok(files.manifest.request.collection().source_streams().iter().all(|stream| {
                files
                    .manifest
                    .files
                    .iter()
                    .any(|file| file.stream == *stream && file.status == CollectionStatus::Complete)
            }))
        })
        .await?;
    if complete {
        let completion = source.finish().await?;
        if completion.is_verified() {
            files
                .apply(move |files| {
                    files.confirm_source(completion)?;
                    if files.manifest.request.collection() == culprit_core::telemetry::TelemetryCollection::Activity {
                        let before = baseline
                            .as_ref()
                            .and_then(|baseline| {
                                files.manifest.description.as_ref().map(|source| baseline.matching(source))
                            })
                            .unwrap_or(&[]);
                        files.derive_activity(before)?;
                    }
                    Ok(())
                })
                .await?;
        } else {
            files
                .apply(|files| {
                    files.invalidate(CollectionStop::SourceChanged);
                    Ok(())
                })
                .await?;
        }
    }
    Ok(failure)
}

async fn collect_stream(
    source: &mut dyn TelemetrySource,
    files: &ObservationWriter,
    index: usize,
    stream: TelemetryStream,
) -> Result<Option<CollectionStop>> {
    let mut cursor: Option<String> = None;
    let mut visited = HashSet::new();
    loop {
        if let Err(reason) = files.apply(move |files| Ok(files.begin_page(index))).await? {
            return Ok(Some(reason));
        }
        match source.read_page(stream, cursor.as_deref()).await? {
            CollectionRead::Stopped(reason) => return Ok(Some(reason)),
            CollectionRead::Page { records, next_cursor } => {
                if let Some(reason) = files.apply(move |files| files.write_page(index, &records)).await? {
                    return Ok(Some(reason));
                }
                let Some(next) = next_cursor else {
                    return Ok(None);
                };
                if next.is_empty()
                    || next.len() > culprit_core::telemetry::MAX_CURSOR_BYTES
                    || !visited.insert(next.clone())
                {
                    return Ok(Some(CollectionStop::InvalidPage));
                }
                cursor = Some(next);
            }
        }
    }
}

#[cfg(test)]
mod tests;
