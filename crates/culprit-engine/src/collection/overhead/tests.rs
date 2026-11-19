use super::*;
use culprit_collector::capture::{CollectionCancellation, SourceFactory, collect};
use culprit_core::{Error, ErrorCode, telemetry::*};
use culprit_read::admission::SourceGate;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

const PRIVATE_PREVIEW: &str =
    "Authorization: Bearer private-session-token /private/native-endpoint original_activity_sql";

struct FailingSource {
    unconfirmed: bool,
    calls: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl SourceFactory for FailingSource {
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Self { unconfirmed: self.unconfirmed, calls: self.calls.clone() }))
    }
}

#[async_trait::async_trait]
impl TelemetrySource for FailingSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::new(ErrorCode::PermissionDenied, "controlled native describe failure"))
    }
    fn query_count(&self) -> u32 {
        1
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        Some(ReadWorkMetrics {
            native_session_work: None,
            active_tasks: u32::from(self.unconfirmed),
            untracked_tasks: 1,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"controlled exact native template"),
                statement: DefinitionPayload::Available {
                    text: PRIVATE_PREVIEW.into(),
                    sha256: digest(PRIVATE_PREVIEW.as_bytes()),
                    redaction_version: REDACTION_VERSION,
                },
                completed_tasks: 1,
                failures: 1,
                timeouts: 0,
                cancellations: u32::from(self.unconfirmed),
                unconfirmed_cancellations: u32::from(self.unconfirmed),
                stop_causes: vec![],
                rows_decoded: "9007199254740993".into(),
                charged_value_bytes: "18446744073709551615".into(),
                client_elapsed_ms: 1.0,
            }],
        })
    }
    async fn read_page(&mut self, _: TelemetryStream, _: Option<&str>) -> Result<CollectionRead> {
        panic!("a failed description cannot read native rows")
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        panic!("a failed description cannot verify a native frame")
    }
    async fn close(&mut self) -> SourceShutdown {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unconfirmed { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
}

#[tokio::test]
async fn failed_describe_and_unsettled_attempts_are_visible_without_credentials_payload_reads_or_dialect_guessing() {
    for unconfirmed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox");
        let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
            budget: CollectionBudget::default(),
        };
        let mut lease = outbox.reserve("private-saved-connection-canary", &request).await.unwrap();
        let capture = lease.identity().clone();
        let directory = lease.directory().to_owned();
        let calls = Arc::new(AtomicU32::new(0));
        let result = collect(
            &SourceGate::default(),
            &FailingSource { unconfirmed, calls: calls.clone() },
            request,
            &directory,
            capture.clone(),
            &CollectionCancellation::default(),
            Some(&mut lease),
        )
        .await;
        if unconfirmed {
            assert_eq!(result.unwrap().status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
        }
        lease.finish().await.unwrap();
        let before = calls.load(Ordering::SeqCst);
        let report = inspect_source_overhead(&path, &capture.observation_id).await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), before);
        assert_eq!(before, 3, "one open, original failed description and same owner close, with no retry");
        assert_eq!(report.evidence.capture, capture);
        assert_eq!(report.evidence.engine, None);
        assert_eq!(report.evidence.source_queries, Some(1));
        assert!(matches!(report.evidence.status, CollectionStatus::Incomplete(_)));
        let metrics = report.evidence.read_metrics.as_ref().unwrap();
        assert_eq!(metrics.active_tasks, u32::from(unconfirmed));
        assert_eq!(metrics.untracked_tasks, 1);
        assert_eq!(metrics.templates[0].unconfirmed_cancellations, u32::from(unconfirmed));
        assert_eq!(metrics.templates[0].rows_decoded, "9007199254740993");
        assert_eq!(metrics.templates[0].charged_value_bytes, "18446744073709551615");
        assert!(matches!(
            metrics.templates[0].statement,
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
        ));
        let encoded = serde_json::to_string(&report).unwrap();
        for private in [
            PRIVATE_PREVIEW,
            "private-session-token",
            "native-endpoint",
            "original_activity_sql",
            "private-saved-connection-canary",
        ] {
            assert!(!encoded.contains(private));
        }
        let status = outbox.status(10, None).await.unwrap();
        assert_eq!(status.ready, 0);
        assert_eq!(status.source_debts, u32::from(unconfirmed));
        assert_eq!(status.entries[0].state, OutboxCaptureState::Incomplete);
        let original = Outbox::inspect_manifest(&path, &capture.observation_id).await.unwrap().unwrap();
        assert!(
            matches!(&original.read_metrics.as_ref().unwrap().templates[0].statement,
            DefinitionPayload::Available { text, .. } if text == PRIVATE_PREVIEW),
            "inspection never changes the private original accounting payload"
        );
    }
}

#[tokio::test]
async fn an_allocation_without_original_clock_is_not_a_report_and_unknown_query_total_is_not_zero() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let lease = outbox.reserve("fixture", &request).await.unwrap();
    let capture = lease.identity().clone();
    assert!(inspect_source_overhead(&path, &capture.observation_id).await.unwrap().is_none());
    lease.finish().await.unwrap();
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
}

struct PendingOpen {
    entered: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl SourceFactory for PendingOpen {
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn unfinished_client_acquisition_has_an_unknown_total_and_final_cancelled_zero_stays_zero() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let mut lease = outbox.reserve("no-source-identity", &request).await.unwrap();
    let identity = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let cancellation = CollectionCancellation::default();
    let pending = PendingOpen { entered: tokio::sync::Notify::new() };
    let gate = SourceGate::default();
    let manifest = {
        let acquiring =
            collect(&gate, &pending, request, &directory, identity.clone(), &cancellation, Some(&mut lease));
        tokio::pin!(acquiring);
        tokio::select! {
            result = &mut acquiring => panic!("controlled acquisition must still be pending: {result:?}"),
            _ = pending.entered.notified() => (),
        }
        let report = inspect_source_overhead(&path, &identity.observation_id).await.unwrap().unwrap();
        assert_eq!(report.evidence.status, CollectionStatus::Incomplete(CollectionStop::Pending));
        assert_eq!(report.evidence.source_queries, None);
        assert_eq!(report.evidence.engine, None);
        assert!(report.evidence.finished_at.is_none());
        assert!(report.evidence.read_metrics.is_none());
        cancellation.cancel();
        acquiring.await.unwrap()
    };
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    lease.finish().await.unwrap();
    let report = inspect_source_overhead(&path, &identity.observation_id).await.unwrap().unwrap();
    assert_eq!(report.evidence.source_queries, Some(0));
    assert!(report.evidence.finished_at.is_some());
    assert_eq!(report.evidence.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
}
