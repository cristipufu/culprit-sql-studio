use super::*;
use culprit_core::telemetry::*;
use std::sync::Arc;

#[derive(Default)]
struct Lifecycle {
    entered: tokio::sync::Notify,
    closed: tokio::sync::Notify,
}
struct Factory(Arc<Lifecycle>);
struct Source(Arc<Lifecycle>);

#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(Source(self.0.clone())))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for Source {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.0.entered.notify_one();
        Ok(CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "fixture",
                "localhost",
                1433,
                None,
            )
            .into(),
            source: "fixture".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: "2020-01-01T06:00:00Z".into(),
                sources: vec!["fixture".into()],
            }),
        })
    }
    fn query_count(&self) -> u32 {
        1
    }
    async fn read_page(&mut self, _: TelemetryStream, _: Option<&str>) -> Result<CollectionRead> {
        panic!("cancelled storage must not admit another source read")
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        panic!("cancelled storage must not verify the source")
    }
    async fn close(&mut self) -> SourceShutdown {
        self.0.closed.notify_one();
        SourceShutdown::Quiescent
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_closes_native_work_before_waiting_for_stalled_artifact_io() {
    let directory = tempfile::tempdir().unwrap();
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget::default(),
    };
    let files = ObservationWriter::create(
        directory.path().join("capture"),
        request.clone(),
        culprit_read::admission::observation_identity().unwrap(),
    )
    .await
    .unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let pending = files.clone();
    let writing = tokio::spawn(async move {
        pending
            .apply(move |_| {
                let _ = entered.send(());
                let _ = blocked.recv();
                Ok(())
            })
            .await
    });
    waiting.await.unwrap();
    let state = Arc::new(Lifecycle::default());
    let factory = Factory(state.clone());
    let cancellation = CollectionCancellation::default();
    let stop = cancellation.clone();
    let capture =
        tokio::spawn(
            async move { collect_prepared(&SourceGate::default(), &factory, request, files, &stop, None).await },
        );
    state.entered.notified().await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(500), state.closed.notified())
        .await
        .expect("native closure must not wait for the stalled filesystem");
    assert!(!capture.is_finished(), "the incomplete artifact is still waiting for its storage owner");
    release.send(()).unwrap();
    writing.await.unwrap().unwrap();
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
}
