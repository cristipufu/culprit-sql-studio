//! Blocking artifact I/O has one owner and cannot occupy the runtime that cancels native source work.
use crate::collection_files::ObservationFiles;
use culprit_core::{
    Result,
    telemetry::{CaptureIdentity, CollectionManifest, CollectionRequest, CollectionStop, ReadWorkMetrics},
};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub(super) struct ObservationWriter {
    files: Arc<Mutex<Option<ObservationFiles>>>,
}
impl ObservationWriter {
    pub async fn create(directory: PathBuf, request: CollectionRequest, capture: CaptureIdentity) -> Result<Self> {
        let files = tokio::task::spawn_blocking(move || ObservationFiles::create(&directory, request, capture))
            .await
            .map_err(|_| CollectionStop::StorageFailed.error())??;
        Ok(Self { files: Arc::new(Mutex::new(Some(files))) })
    }
    pub async fn apply<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ObservationFiles) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        // Take ownership before dispatch. Cancelling the caller cannot reorder a pending write after finalization.
        let mut files = self.files.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            operation(files.as_mut().ok_or_else(|| CollectionStop::StorageFailed.error())?)
        })
        .await
        .map_err(|_| CollectionStop::StorageFailed.error())?
    }
    pub async fn finish(
        &self,
        queries: u32,
        metrics: Option<ReadWorkMetrics>,
        stop: Option<CollectionStop>,
    ) -> Result<CollectionManifest> {
        let mut files = self.files.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let mut files = files.take().ok_or_else(|| CollectionStop::StorageFailed.error())?;
            files.manifest.read_metrics = metrics;
            if let Some(reason) = stop {
                files.invalidate(reason);
            }
            files.finish(queries)
        })
        .await
        .map_err(|_| CollectionStop::StorageFailed.error())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::{CollectionBudget, CollectionSelection, CollectionStatus};

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_storage_keeps_cancellation_timers_live_and_cannot_write_after_finalization() {
        let directory = tempfile::tempdir().unwrap();
        let request = CollectionRequest {
            selection: CollectionSelection::QueryHistory {
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            budget: CollectionBudget::default(),
        };
        let writer = ObservationWriter::create(
            directory.path().join("capture"),
            request,
            culprit_read::admission::observation_identity().unwrap(),
        )
        .await
        .unwrap();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let pending = writer.clone();
        let writing = tokio::spawn(async move {
            pending
                .apply(move |files| {
                    let _ = entered.send(());
                    let _ = blocked.recv();
                    files.manifest.source_queries = 17;
                    Ok(())
                })
                .await
        });
        waiting.await.unwrap();
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());
        let finishing = writer.finish(23, None, Some(CollectionStop::Cancelled));
        tokio::pin!(finishing);
        // A blocked filesystem operation must not stall this current-thread runtime or let finish overtake it.
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut finishing).await.is_err());
        release.send(()).unwrap();
        let result = finishing.await.unwrap();
        assert_eq!(result.source_queries, 23);
        assert_eq!(result.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
        assert!(writer.apply(|_| Ok(())).await.is_err());
    }
}
