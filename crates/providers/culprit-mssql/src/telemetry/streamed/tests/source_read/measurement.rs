//! The measuring purpose uses the actual reader/capture owner. Scripted native values prove wiring, not cost.
use super::*;
use crate::read::session_work::{Observation, SessionWork};
use culprit_collector::capture::{self, CollectionCancellation, SourceFactory};
use culprit_core::{RecipeQualificationAssessment, RecipeQualificationGap};
use std::sync::Mutex;

struct RecordedStream {
    native: StreamingRead,
    work: SessionWork,
}
#[async_trait::async_trait]
impl NativeRead for RecordedStream {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(1)
    }
    fn observe_session_work(&mut self, observation: Observation) {
        self.work.observe(observation);
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        Some(ReadWorkMetrics {
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![],
            native_session_work: self.work.snapshot(),
        })
    }
    async fn close(&mut self) -> SourceShutdown {
        let shutdown = self.native.close().await;
        self.work.close(shutdown, 1);
        shutdown
    }
}
#[async_trait::async_trait]
impl NativeStreamRead for RecordedStream {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<()> {
        self.native.begin_stream(sql, parameters, limit).await
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        self.native.next_batch().await
    }
}
type MeasurementSource = StreamedSnapshot<QueryCompilation, RecordedStream>;
struct OwnedSource(Mutex<Option<MeasurementSource>>);
#[async_trait::async_trait]
impl SourceFactory for OwnedSource {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        assert!(matches!(
            request.selection,
            CollectionSelection::Snapshot { dataset: SnapshotDataset::QueryCompilation }
        ));
        Ok(Box::new(self.0.lock().unwrap().take().unwrap()))
    }
}
fn measurement_request() -> CollectionRequest {
    CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::QueryCompilation },
        budget: CollectionBudget { max_queries: 8, ..Default::default() },
        ..request()
    }
}
fn measured_authority(at: &str, cpu: i64) -> Rec {
    let original = authority(at);
    let mut fields = original.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("collector_session_id", Val::Int(71)),
        ("collector_login_without_offset", Val::Text("2020-01-01T05:30:00.003".into())),
        ("collector_cpu_ms", Val::Int(cpu)),
        ("collector_reads", Val::Int(9_007_199_254_740_993)),
        ("collector_logical_reads", Val::Int(200 + cpu)),
        ("collector_dirty_marks", Val::Int(3)),
        ("collector_memory_pages", Val::Int(32)),
    ]);
    Rec::of(&fields)
}
fn measured(before: Rec, after: Rec, shutdown: SourceShutdown) -> MeasurementSource {
    measurement(native(before, after, shutdown), &measurement_request())
}
fn measurement(native: StreamingRead, request: &CollectionRequest) -> MeasurementSource {
    let io = RecordedStream { native, work: SessionWork::default() };
    StreamedSnapshot::with_purpose(
        io,
        request,
        "fixture.invalid".into(),
        1433,
        QueryCompilation,
        SnapshotPurpose::NativeCostMeasurement,
    )
    .unwrap()
}

#[tokio::test]
async fn measurement_capture_uses_eight_original_queries_and_real_same_reader_counters_without_incident_lineage() {
    let source = measured(measured_authority(FIRST, 10), measured_authority(LAST, 17), SourceShutdown::Quiescent);
    let manifest = capture_measurement(source).await;
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert_eq!(manifest.source_queries, 8);
    assert!(manifest.investigation.is_none());
    let source = manifest.description.as_ref().unwrap();
    assert_eq!(source.partition, SourcePartition::Target);
    assert!(matches!(source.observation, SourceObservation::Snapshot(_)));
    assert!(source.observation.execution_context().is_none());
    let metrics = manifest.read_metrics.as_ref().unwrap();
    let NativeSessionReadWork::MssqlSession(work) = metrics.native_session_work.as_deref().unwrap();
    assert_eq!(work.settlement, NativeReadSettlement::Quiescent);
    assert_eq!(work.issue, None);
    let interval = work.interval.as_ref().unwrap();
    assert!(interval.context.has_qualified_local_owner());
    assert_eq!(interval.initial.query_ordinal, 2);
    assert_eq!(interval.latest.as_ref().unwrap().query_ordinal, 8);
    assert_eq!(interval.initial.cpu_ms.as_deref(), Some("10"));
    assert_eq!(interval.latest.as_ref().unwrap().cpu_ms.as_deref(), Some("17"));
    let RecipeQualificationAssessment::Refuse(gaps) = crate::assess_recipe_qualification(&manifest, source).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        gaps,
        [
            RecipeQualificationGap::PartialNativeMeasurementWindow,
            RecipeQualificationGap::NativeGrantEvidenceUnavailable,
            RecipeQualificationGap::ActiveCancellationUnproved,
        ]
    );
    let public = serde_json::to_string(&metrics.summary(Some(source)).unwrap()).unwrap();
    assert!(
        !public.contains("reader_generation")
            && !public.contains("collector_login")
            && !public.contains("fixture.invalid")
    );
}

async fn capture_measurement(source: MeasurementSource) -> CollectionManifest {
    let factory = OwnedSource(Mutex::new(Some(source)));
    let directory = tempfile::tempdir().unwrap();
    capture::collect_once(
        &culprit_read::admission::SourceGate::default(),
        &factory,
        measurement_request(),
        &directory.path().join("capture"),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn optional_self_counters_never_break_source_data_or_mint_native_measurements() {
    let manifest = capture_measurement(measured(authority(FIRST), authority(LAST), SourceShutdown::Quiescent)).await;
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert_eq!(manifest.source_queries, 8);
    let source = manifest.description.as_ref().unwrap();
    assert_eq!(source.observation.snapshot().unwrap().records, 1);
    let NativeSessionReadWork::MssqlSession(work) =
        manifest.read_metrics.as_ref().unwrap().native_session_work.as_deref().unwrap();
    assert_eq!(work.settlement, NativeReadSettlement::Quiescent);
    assert_eq!(work.issue, Some(NativeReadWorkIssue::MissingSession));
    assert!(work.interval.is_none());
    let RecipeQualificationAssessment::Refuse(gaps) = crate::assess_recipe_qualification(&manifest, source).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        gaps,
        [
            RecipeQualificationGap::NativeMeasurementsUnavailable,
            RecipeQualificationGap::NativeGrantEvidenceUnavailable,
            RecipeQualificationGap::ActiveCancellationUnproved,
        ]
    );
}

#[tokio::test]
async fn a_changed_owner_or_uncertain_close_cannot_turn_measurement_into_settled_native_evidence() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let after = replace(&measured_authority(LAST, 17), &[("local_process_id", Val::Int(12))]);
        let mut source = measured(measured_authority(FIRST, 10), after, shutdown);
        source.describe().await.unwrap();
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, shutdown);
        let metrics = source.read_metrics().unwrap();
        let NativeSessionReadWork::MssqlSession(work) = metrics.native_session_work.as_deref().unwrap();
        assert_eq!(work.issue, Some(NativeReadWorkIssue::SourceChanged));
        assert_eq!(
            work.settlement,
            match shutdown {
                SourceShutdown::Quiescent => NativeReadSettlement::Quiescent,
                SourceShutdown::Unconfirmed => NativeReadSettlement::Unconfirmed,
            }
        );
    }
}

#[tokio::test]
async fn unsupported_native_profile_and_stricter_query_budget_dispatch_no_body_or_rescue() {
    let mut io = native(measured_authority(FIRST, 10), measured_authority(LAST, 17), SourceShutdown::Quiescent);
    io.native = FakeRead::new(vec![Ok(vec![Rec::of(&[("edition", Val::Int(3))])])]);
    let mut boxed = measurement(io, &measurement_request());
    assert_eq!(boxed.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(boxed.query_count(), 1);
    assert_eq!(boxed.inner.reader().io.native.starts, 0);
    assert_eq!(boxed.close().await, SourceShutdown::Quiescent);

    let mut io = native(measured_authority(FIRST, 10), measured_authority(LAST, 17), SourceShutdown::Quiescent);
    io.native = io.native.with_query_limit(7);
    let mut request = measurement_request();
    request.budget.max_queries = 7;
    let mut bounded = measurement(io, &request);
    bounded.describe().await.unwrap();
    bounded.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(CollectionStop::from_error(&bounded.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(bounded.query_count(), 7);
    assert_eq!(bounded.inner.reader().io.native.starts, 1);
    assert_eq!(bounded.close().await, SourceShutdown::Quiescent);
}
