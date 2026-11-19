//! Scripted native lifecycle only; owned provider fixtures separately establish live compatibility.
use super::*;
use crate::{analytical::test_support, outbox::Outbox};
use culprit_read::{ReadBatch, SnapshotReader, SnapshotRecheck, SnapshotRecord, SnapshotScope, SnapshotSource};
use std::{collections::VecDeque, path::PathBuf, sync::Mutex};

#[derive(Clone, Copy)]
enum Mode {
    Complete,
    LateClock,
    LateError,
    CancelAfterPrefix,
    ChangeBytes,
}

struct Script {
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
    mode: Mode,
    directory: PathBuf,
    cancellation: CollectionCancellation,
    calls: Arc<Mutex<Vec<&'static str>>>,
}
struct Reader {
    script: Arc<Script>,
    pending: VecDeque<TelemetryRecord>,
}
impl Reader {
    fn called(&self, name: &'static str) {
        self.script.calls.lock().unwrap().push(name);
    }
}
#[async_trait::async_trait]
impl SourceFactory for Arc<Script> {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(SnapshotSource::new(Reader { script: self.clone(), pending: self.rows.clone().into() }, request)?))
    }
}
#[async_trait::async_trait]
impl SnapshotReader for Reader {
    type Row = TelemetryRecord;
    async fn describe(&mut self, _: &CollectionRequest) -> Result<CollectionDescription> {
        self.called("describe");
        let mut source = self.script.source.clone();
        let snapshot = source.observation.snapshot().unwrap();
        source.observation = SourceObservation::PagedSnapshot(PagedSnapshotDescription {
            dataset: snapshot.dataset,
            observed_from: snapshot.observed_from.clone(),
            sources: snapshot.sources.iter().map(|scope| scope.source.clone()).collect(),
        });
        Ok(source)
    }
    async fn begin(&mut self, _: &CollectionRequest) -> Result<()> {
        self.called("begin");
        Ok(())
    }
    async fn next_batch(&mut self) -> Result<ReadBatch<Self::Row>> {
        self.called("next");
        match self.pending.pop_front() {
            Some(row) => Ok(ReadBatch {
                rows: vec![row],
                complete: self.pending.is_empty()
                    && !matches!(self.script.mode, Mode::LateError | Mode::CancelAfterPrefix),
            }),
            None if matches!(self.script.mode, Mode::LateError) => Err(CollectionStop::SourceUnavailable.error()),
            None if matches!(self.script.mode, Mode::CancelAfterPrefix) => {
                self.script.cancellation.cancel();
                std::future::pending().await
            }
            None => panic!("the completed stream must not be read again"),
        }
    }
    fn decode(&self, record: &Self::Row, _: &CollectionDescription) -> Result<SnapshotRecord> {
        let TelemetryRecord::Operational(row) = record else { panic!() };
        Ok(SnapshotRecord { source: row.source.clone(), record: record.clone(), complete_scope: true })
    }
    async fn recheck(&mut self, _: &CollectionDescription) -> Result<SnapshotRecheck> {
        self.called("recheck");
        let snapshot = self.script.source.observation.snapshot().unwrap();
        Ok(SnapshotRecheck {
            unchanged: true,
            observed_until: snapshot.observed_until.clone(),
            execution: None,
            scopes: snapshot.sources.iter().map(|scope| SnapshotScope::complete(&scope.source)).collect(),
        })
    }
    fn query_count(&self) -> u32 {
        self.script
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|name| matches!(**name, "describe" | "begin" | "recheck"))
            .count() as u32
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.called("close");
        if matches!(self.script.mode, Mode::ChangeBytes) {
            // Only final validation, after this same close, can observe the changed retained file.
            let path = self.script.directory.join(TelemetryStream::OperationalSamples.file_name());
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b'x');
            std::fs::write(path, bytes).unwrap();
        }
        SourceShutdown::Quiescent
    }
}

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::StatementCounters },
        budget: CollectionBudget { page_rows: 1, max_snapshot_rows: 8, ..Default::default() },
    }
}
struct Captured {
    _root: tempfile::TempDir,
    directory: PathBuf,
    manifest: CollectionManifest,
    calls: Vec<&'static str>,
    ready: Option<u32>,
}
async fn capture(mode: Mode, into_outbox: bool) -> Captured {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = into_outbox.then(|| Outbox::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap());
    let mut lease = match outbox.as_mut() {
        Some(outbox) => Some(outbox.reserve("fixture", &request()).await.unwrap()),
        None => None,
    };
    let directory = lease.as_ref().map_or_else(|| root.path().join("capture"), |lease| lease.directory().to_owned());
    let identity = lease
        .as_ref()
        .map_or_else(|| culprit_read::admission::observation_identity().unwrap(), |lease| lease.identity().clone());
    let (source, mut rows) = test_support::statement_work_evidence_cases().remove(0);
    if matches!(mode, Mode::LateClock) {
        let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
        let OperationalFact::StatementCounters(point) = &mut row.fact else { panic!() };
        point.observed_at = "2020-01-01T12:00:02Z".into();
    }
    if matches!(mode, Mode::LateError | Mode::CancelAfterPrefix) {
        rows.truncate(1);
    }
    let script = Arc::new(Script {
        source,
        rows,
        mode,
        directory: directory.clone(),
        cancellation: CollectionCancellation::default(),
        calls: Arc::default(),
    });
    let result =
        collect(&SourceGate::default(), &script, request(), &directory, identity, &script.cancellation, lease.as_mut())
            .await;
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) if matches!(mode, Mode::LateError) => {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceUnavailable);
            serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap()
        }
        Err(error) => panic!("scripted capture failed unexpectedly: {error:?}"),
    };
    if let Some(lease) = lease {
        lease.finish().await.unwrap();
    }
    let ready = match outbox {
        Some(outbox) => Some(outbox.status(8, None).await.unwrap().ready),
        None => None,
    };
    let calls = script.calls.lock().unwrap().clone();
    Captured { _root: root, directory, manifest, calls, ready }
}

#[tokio::test]
async fn paged_statement_rows_reach_a_strict_final_capture_in_collect_once_and_outbox() {
    for outbox in [false, true] {
        let capture = capture(Mode::Complete, outbox).await;
        assert_eq!(capture.manifest.status, CollectionStatus::Complete);
        assert_eq!(capture.manifest.files[0].rows, 2);
        assert_eq!(capture.calls, ["describe", "begin", "next", "next", "recheck", "close"]);
        assert_eq!(capture.manifest.source_queries, 3, "final disk verification cannot requery the native source");
        assert_eq!(capture.ready, outbox.then_some(1));
    }
}

#[tokio::test]
async fn a_native_point_after_the_final_native_clock_never_becomes_complete_or_ready() {
    for outbox in [false, true] {
        let capture = capture(Mode::LateClock, outbox).await;
        assert_eq!(capture.manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
        assert_eq!(capture.manifest.files[0].rows, 2, "provisional original evidence remains retained");
        assert!(!capture.manifest.references.integrity_complete);
        assert_eq!(capture.calls.last(), Some(&"close"));
        assert_eq!(capture.ready, outbox.then_some(0));
    }
}

#[tokio::test]
async fn late_errors_and_caller_cancellation_keep_the_positive_prefix_without_final_verification_or_replacement() {
    for mode in [Mode::LateError, Mode::CancelAfterPrefix] {
        for outbox in [false, true] {
            let capture = capture(mode, outbox).await;
            assert!(matches!(capture.manifest.status, CollectionStatus::Incomplete(_)));
            assert_eq!(capture.manifest.files[0].rows, 1);
            assert_eq!(capture.calls, ["describe", "begin", "next", "next", "close"]);
            assert!(!capture.manifest.references.integrity_complete);
            let path = capture.directory.join(&capture.manifest.files[0].file);
            let row: ExportedRecord = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert!(matches!(row.record, TelemetryRecord::Operational(_)));
            assert_eq!(capture.ready, outbox.then_some(0));
        }
    }
}

#[tokio::test]
async fn final_disk_proof_runs_after_the_original_close_and_rejects_changed_bytes_for_both_capture_routes() {
    for outbox in [false, true] {
        let capture = capture(Mode::ChangeBytes, outbox).await;
        assert_eq!(capture.manifest.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
        assert_eq!(capture.manifest.files[0].rows, 2);
        assert_eq!(capture.calls.iter().filter(|call| **call == "close").count(), 1);
        assert_eq!(capture.ready, outbox.then_some(0));
    }
}
