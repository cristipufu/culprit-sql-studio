use super::*;
use crate::analytical::test_support::investigation_fixture;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

#[derive(Clone, Copy)]
enum FailureStage {
    AuthenticationBlocked,
    AuthenticationFailed,
    DescribeBlocked,
    DescribeFailed,
}
#[derive(Default)]
struct Boundaries {
    owner_reads: AtomicU32,
    opens: AtomicU32,
    descriptions: AtomicU32,
    closes: AtomicU32,
    entered: tokio::sync::Notify,
}
struct AdmittedQuestion {
    intent: DiagnosticInvestigationIntent,
    output: PathBuf,
    stage: FailureStage,
    boundaries: Arc<Boundaries>,
    window: Option<CollectionAdmissionWindow>,
}
struct FailingSource {
    question: DiagnosticInvestigationIntent,
    output: PathBuf,
    stage: FailureStage,
    boundaries: Arc<Boundaries>,
}
fn initial_manifest(output: &Path) -> CollectionManifest {
    serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap()
}
fn assert_owner(manifest: &CollectionManifest, expected: &DiagnosticInvestigationIntent) {
    assert_eq!(manifest.investigation_intent().unwrap().as_ref(), Some(expected));
    assert!(manifest.investigation.as_ref().unwrap().matches_request(&manifest.request));
}
fn question(output: PathBuf, stage: FailureStage) -> AdmittedQuestion {
    AdmittedQuestion {
        intent: investigation_fixture().intent,
        output,
        stage,
        boundaries: Arc::new(Boundaries::default()),
        window: None,
    }
}
fn request(question: &DiagnosticInvestigationIntent) -> CollectionRequest {
    let value = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(question.focus.clone()) },
        budget: question.budget.clone(),
    };
    value.validate().unwrap();
    value
}
#[async_trait::async_trait]
impl SourceFactory for AdmittedQuestion {
    fn admission_window(&self) -> Option<CollectionAdmissionWindow> {
        self.window
    }
    fn investigation_intent(&self) -> Option<&DiagnosticInvestigationIntent> {
        self.boundaries.owner_reads.fetch_add(1, Ordering::SeqCst);
        Some(&self.intent)
    }
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.boundaries.opens.fetch_add(1, Ordering::SeqCst);
        let manifest = initial_manifest(&self.output);
        assert_owner(&manifest, &self.intent);
        assert!(manifest.description.is_none());
        assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Pending));
        match self.stage {
            FailureStage::AuthenticationBlocked => {
                self.boundaries.entered.notify_one();
                std::future::pending().await
            }
            FailureStage::AuthenticationFailed => {
                Err(Error::new(culprit_core::ErrorCode::AuthFailed, "fixture authentication denied"))
            }
            FailureStage::DescribeBlocked | FailureStage::DescribeFailed => Ok(Box::new(FailingSource {
                question: self.intent.clone(),
                output: self.output.clone(),
                stage: self.stage,
                boundaries: self.boundaries.clone(),
            })),
        }
    }
}
#[async_trait::async_trait]
impl TelemetrySource for FailingSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        let manifest = initial_manifest(&self.output);
        assert_owner(&manifest, &self.question);
        assert!(manifest.description.is_none());
        self.boundaries.descriptions.fetch_add(1, Ordering::SeqCst);
        self.boundaries.entered.notify_one();
        if matches!(self.stage, FailureStage::DescribeBlocked) {
            std::future::pending().await
        } else {
            Err(Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture native metadata denied"))
        }
    }
    fn query_count(&self) -> u32 {
        self.boundaries.descriptions.load(Ordering::SeqCst)
    }
    async fn read_page(&mut self, _: TelemetryStream, _: Option<&str>) -> Result<CollectionRead> {
        panic!("failed describe cannot admit paging")
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        panic!("failed describe cannot verify a native source")
    }
    async fn close(&mut self) -> SourceShutdown {
        self.boundaries.closes.fetch_add(1, Ordering::SeqCst);
        SourceShutdown::Quiescent
    }
}

#[tokio::test]
async fn initial_owner_manifest_precedes_blocked_authentication_and_survives_cancellation_without_source_facts() {
    let root = tempfile::tempdir().unwrap();
    let factory = question(root.path().join("capture"), FailureStage::AuthenticationBlocked);
    let cancellation = CollectionCancellation::default();
    let gate = SourceGate::default();
    let capture = collect_once(&gate, &factory, request(&factory.intent), &factory.output, &cancellation);
    tokio::pin!(capture);
    tokio::select! { biased; _ = &mut capture => panic!("authentication must still be pending"), _ = factory.boundaries.entered.notified() => {} }
    let initial = initial_manifest(&factory.output);
    assert_owner(&initial, &factory.intent);
    assert!(initial.description.is_none() && initial.finished_at.is_none());
    assert_eq!(initial.source_queries, 0);
    cancellation.cancel();
    let manifest = capture.await.unwrap();
    assert_owner(&manifest, &factory.intent);
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    assert!(manifest.description.is_none());
    assert_eq!(
        factory.boundaries.owner_reads.load(Ordering::SeqCst),
        1,
        "one immutable owner value supplies the complete capture"
    );
    assert_eq!(factory.boundaries.descriptions.load(Ordering::SeqCst), 0);
    assert_eq!(factory.boundaries.closes.load(Ordering::SeqCst), 0);
    assert!(!gate.is_closed());
}

#[tokio::test]
async fn authentication_error_preserves_the_exact_admitted_question_without_inventing_a_source() {
    let root = tempfile::tempdir().unwrap();
    let factory = question(root.path().join("capture"), FailureStage::AuthenticationFailed);
    let error = collect_once(
        &SourceGate::default(),
        &factory,
        request(&factory.intent),
        &factory.output,
        &CollectionCancellation::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::AuthFailed);
    let manifest = initial_manifest(&factory.output);
    assert_owner(&manifest, &factory.intent);
    assert!(manifest.description.is_none());
    assert!(manifest.finished_at.is_some());
    assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
    assert_eq!(manifest.source_queries, 0);
    assert_eq!(factory.boundaries.owner_reads.load(Ordering::SeqCst), 1);
    assert_eq!(factory.boundaries.descriptions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_describe_error_or_cancellation_keeps_owner_lineage_and_confirms_source_close() {
    for stage in [FailureStage::DescribeBlocked, FailureStage::DescribeFailed] {
        let root = tempfile::tempdir().unwrap();
        let factory = question(root.path().join("capture"), stage);
        let gate = SourceGate::default();
        let cancellation = CollectionCancellation::default();
        let capture = collect_once(&gate, &factory, request(&factory.intent), &factory.output, &cancellation);
        tokio::pin!(capture);
        if matches!(stage, FailureStage::DescribeBlocked) {
            tokio::select! { biased; _ = &mut capture => panic!("describe must still be pending"), _ = factory.boundaries.entered.notified() => {} }
            let initial = initial_manifest(&factory.output);
            assert_owner(&initial, &factory.intent);
            assert!(initial.description.is_none());
            cancellation.cancel();
            assert_eq!(capture.await.unwrap().status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
        } else {
            assert_eq!(capture.await.unwrap_err().code, culprit_core::ErrorCode::PermissionDenied);
        }
        let manifest = initial_manifest(&factory.output);
        assert_owner(&manifest, &factory.intent);
        assert!(manifest.description.is_none(), "a failed native description remains unavailable");
        assert_eq!(manifest.source_queries, 1);
        assert_eq!(factory.boundaries.owner_reads.load(Ordering::SeqCst), 1);
        assert_eq!(factory.boundaries.closes.load(Ordering::SeqCst), 1);
        assert!(!gate.is_closed());
    }
}

#[tokio::test]
async fn invalid_or_different_admitted_question_and_budget_fail_before_factory_or_artifact_creation() {
    for changed in ["invalid-owner", "budget", "question"] {
        let root = tempfile::tempdir().unwrap();
        let mut factory = question(root.path().join("capture"), FailureStage::AuthenticationFailed);
        let mut requested = request(&factory.intent);
        match changed {
            "invalid-owner" => factory.intent.recipe_version = 0,
            "budget" => requested.budget.max_queries += 1,
            "question" => {
                let CollectionSelection::FocusedPageWait { focus } = &mut requested.selection else { panic!() };
                let original_resource = focus.resource.clone();
                focus.resource.native.id = "7:1:9999".into();
                let observed = focus
                    .original
                    .wait
                    .as_mut()
                    .unwrap()
                    .identified_resources
                    .iter_mut()
                    .find(|resource| **resource == original_resource)
                    .unwrap();
                *observed = focus.resource.clone();
            }
            _ => unreachable!(),
        }
        requested.validate().unwrap();
        let error = collect_once(
            &SourceGate::default(),
            &factory,
            requested,
            &factory.output,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage, "{changed}");
        assert_eq!(factory.boundaries.opens.load(Ordering::SeqCst), 0);
        assert_eq!(factory.boundaries.descriptions.load(Ordering::SeqCst), 0);
        assert!(!factory.output.exists());
    }
}

#[tokio::test]
async fn the_bounded_initial_manifest_rejects_an_oversized_admitted_envelope_before_authentication() {
    let root = tempfile::tempdir().unwrap();
    let mut factory = question(root.path().join("capture"), FailureStage::AuthenticationFailed);
    // Structural validity is not native/cost authority. This exercises an oversized supplied question, not SQL execution.
    factory.intent.focus.original.statement = DefinitionPayload::sql(
        &format!("SELECT {} FROM fixture_metadata", "column_identifier".repeat(10_000)),
        culprit_core::Engine::Mssql,
    );
    assert!(factory.intent.is_valid());
    let requested = request(&factory.intent);
    assert!(serde_json::to_vec(&requested).unwrap().len() > 128 * 1024);
    let error =
        collect_once(&SourceGate::default(), &factory, requested, &factory.output, &CollectionCancellation::default())
            .await
            .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::StorageFailed);
    assert_eq!(factory.boundaries.opens.load(Ordering::SeqCst), 0);
    assert_eq!(factory.boundaries.descriptions.load(Ordering::SeqCst), 0);
    assert!(!factory.output.exists(), "the size check precedes directory or partial-manifest publication");
}

#[tokio::test]
async fn an_expired_admission_keeps_the_initial_owner_manifest_without_opening_a_native_source() {
    let root = tempfile::tempdir().unwrap();
    let mut factory = question(root.path().join("capture"), FailureStage::AuthenticationFailed);
    factory.window =
        Some(CollectionAdmissionWindow::new(Instant::now() - Duration::from_secs(6), &factory.intent.budget).unwrap());
    let manifest = collect_once(
        &SourceGate::default(),
        &factory,
        request(&factory.intent),
        &factory.output,
        &CollectionCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    assert_owner(&manifest, &factory.intent);
    assert!(manifest.description.is_none());
    assert_eq!(factory.boundaries.opens.load(Ordering::SeqCst), 0);
    assert_eq!(factory.boundaries.descriptions.load(Ordering::SeqCst), 0);
    assert_eq!(factory.boundaries.owner_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn alternate_writer_callers_share_request_validation_before_creating_initial_owner_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let intent = investigation_fixture().intent;
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let unrelated = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: intent.budget.clone(),
    };
    let output = root.path().join("capture");
    let error = ObservationWriter::create(
        output.clone(),
        unrelated,
        culprit_read::admission::observation_identity().unwrap(),
        Some(owner),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    assert!(!output.exists());
}
