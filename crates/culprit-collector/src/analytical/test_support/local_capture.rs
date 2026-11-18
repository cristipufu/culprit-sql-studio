//! One real capture owner for local-original consumer conformance. It does not contact a database.
use super::*;
use crate::{
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::Outbox,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The source closes before this fixture exposes a durable result; retaining the fixture retains its temporary files.
pub struct LocalCaptureFixture {
    pub outbox: Option<Outbox>,
    pub directory: PathBuf,
    pub capture_directory: PathBuf,
    pub manifest: CollectionManifest,
    _temporary: tempfile::TempDir,
}
impl LocalCaptureFixture {
    pub async fn query_history(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        assert!(source.query_history().is_some());
        Self::with_shutdown(source, records, SourceShutdown::Quiescent).await
    }

    pub async fn snapshot(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        assert!(source.observation.snapshot().is_some());
        Self::with_shutdown(source, records, SourceShutdown::Quiescent).await
    }

    pub async fn with_shutdown(
        source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        shutdown: SourceShutdown,
    ) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("outbox");
        let mut outbox = Outbox::create(&directory, OutboxLimits::default()).unwrap();
        let selection = match &source.observation {
            SourceObservation::QueryHistory(history) => CollectionSelection::QueryHistory {
                from: history.interval.from.clone(),
                until: history.interval.until.clone(),
            },
            SourceObservation::Snapshot(header) => CollectionSelection::Snapshot { dataset: header.dataset },
            _ => panic!("local fixture requires an original history or snapshot read"),
        };
        let request = CollectionRequest {
            subject: match &source.target {
                CollectionTarget::Database(_) => CollectionSubject::Database,
                CollectionTarget::Shared(resource) => CollectionSubject::Shared { resource: resource.clone() },
            },
            selection,
            budget: CollectionBudget::default(),
        };
        let closed = Arc::new(AtomicBool::new(false));
        let factory = OriginalSource { source, records, shutdown, closed: closed.clone() };
        let mut lease = outbox.reserve("local-evidence", &request).await.unwrap();
        let capture_directory = lease.directory().to_owned();
        let manifest = capture::collect(
            &culprit_read::admission::SourceGate::default(),
            &factory,
            request,
            &capture_directory,
            lease.identity().clone(),
            &CollectionCancellation::default(),
            Some(&mut lease),
        )
        .await
        .unwrap();
        assert!(closed.load(Ordering::SeqCst));
        lease.finish().await.unwrap();
        Self { outbox: Some(outbox), directory, capture_directory, manifest, _temporary: temporary }
    }

    pub fn original_bytes(&self) -> u64 {
        std::fs::metadata(self.capture_directory.join("manifest.json")).unwrap().len()
            + self.manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
    }
}

#[derive(Clone)]
struct OriginalSource {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
    shutdown: SourceShutdown,
    closed: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl SourceFactory for OriginalSource {
    async fn open(&self, request: &CollectionRequest) -> culprit_core::Result<Box<dyn TelemetrySource>> {
        assert!(self.source.matches_request(request));
        Ok(Box::new(self.clone()))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for OriginalSource {
    async fn describe(&mut self) -> culprit_core::Result<CollectionDescription> {
        Ok(self.source.clone())
    }
    fn query_count(&self) -> u32 {
        0
    }
    async fn read_page(
        &mut self,
        stream: TelemetryStream,
        cursor: Option<&str>,
    ) -> culprit_core::Result<CollectionRead> {
        assert!(cursor.is_none());
        Ok(CollectionRead::Page {
            records: self.records.iter().filter(|row| row.stream() == stream).cloned().collect(),
            next_cursor: None,
        })
    }
    async fn finish(&mut self) -> culprit_core::Result<SourceCompletion> {
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown
    }
}
