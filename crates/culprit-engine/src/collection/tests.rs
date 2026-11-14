use super::*;
use culprit_collector::analytical::test_support::{activity_evidence_cases, activity_observation};
use culprit_core::{Engine, ErrorCode, ErrorDetail, ObservedWaitResource, WaitResourceKind, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader, admission::SourceGate};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

#[derive(Clone)]
struct Baseline {
    source: CollectionDescription,
    observation: ActivityObservation,
    native_calls: Arc<AtomicU32>,
}
struct Reader {
    baseline: Baseline,
    queries: u32,
}
#[async_trait::async_trait]
impl SourceFactory for Baseline {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(FrozenSource::new(Reader { baseline: self.clone(), queries: 0 }, request)?))
    }
}
#[async_trait::async_trait]
impl ObservationReader for Reader {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        self.queries += 1;
        self.baseline.native_calls.fetch_add(1, Ordering::SeqCst);
        Ok(FrozenObservation {
            description: self.baseline.source.clone(),
            records: vec![TelemetryRecord::from(self.baseline.observation.clone())],
        })
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        self.queries += 1;
        self.baseline.native_calls.fetch_add(1, Ordering::SeqCst);
        Ok(*source == self.baseline.source)
    }
    fn query_count(&self) -> u32 {
        self.queries
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        SourceShutdown::Quiescent
    }
}

async fn retained_parent(path: &Path) -> (Outbox, ActivityWaitFocus, Arc<AtomicU32>) {
    let mut outbox = Outbox::create(path, OutboxLimits::default()).unwrap();
    outbox.recover().await.unwrap();
    let (mut source, _) = activity_evidence_cases().remove(0);
    let SourceObservation::Activity(snapshot) = &mut source.observation else { panic!() };
    snapshot.native_rows = 1;
    snapshot.observations = 1;
    snapshot.coverage = StateSourceCoverage::collected(source.source.clone(), 1);
    let mut observation = activity_observation(DefinitionPayload::sql(
        "SELECT payload FROM dbo.orders WHERE tenant_id=9264",
        Engine::Mssql,
    ));
    let resource = ObservedWaitResource { kind: WaitResourceKind::MemoryPage, native: NativeIdentity::new("7:1:4144") };
    observation.wait = Some(ActivityWait {
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1".into()),
        identified_resources: vec![resource.clone()],
        owners: vec![],
        resources: None,
    });
    let native_calls = Arc::new(AtomicU32::new(0));
    let factory = Baseline { source, observation, native_calls: native_calls.clone() };
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget {
            max_queries: 2,
            max_pages: 1,
            max_snapshot_rows: 1,
            page_rows: 1,
            ..Default::default()
        },
    };
    let mut lease = outbox.reserve("retained-baseline", &request).await.unwrap();
    let identity = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let manifest = capture::collect(
        &SourceGate::default(),
        &factory,
        request,
        &directory,
        identity.clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    lease.finish().await.unwrap();
    let input = outbox.diagnostic_input(&identity.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    let row = &input.rows[0];
    let TelemetryRecord::Activity(original) = &row.row.record else { panic!() };
    let snapshot = input.source.observation.activity().unwrap();
    let focus = ActivityWaitFocus {
        parent: row.reference.clone(),
        target: input.source.target.clone(),
        source: input.source.source.clone(),
        epoch: snapshot.epoch.clone().unwrap(),
        original_observed_at: snapshot.at.clone(),
        original: original.as_ref().clone(),
        statement_identity: original.statement_identity.clone().unwrap(),
        resource,
    };
    assert!(focus.matches_parent(&input.source, original, &row.reference));
    assert_eq!(native_calls.load(Ordering::SeqCst), 2);
    (outbox, focus, native_calls)
}
fn request(focus: ActivityWaitFocus) -> CollectionRequest {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait { focus: Box::new(focus) },
        budget: CollectionBudget::default(),
    };
    request.validate().unwrap();
    request
}
fn assert_no_workspace_work(workspace: &Workspace, directory: &Path) {
    assert!(!workspace.collection_admission_path().exists(), "native admission must not begin");
    assert!(!workspace.source_gate.is_closed(), "refusing a focus must not fence unrelated future work");
    assert!(!directory.exists(), "route and trust validation must not create output artifacts");
}

#[tokio::test]
async fn forged_original_clock_or_receipt_fails_before_missing_route_or_capture_admission() {
    let root = tempfile::tempdir().unwrap();
    let (mut outbox, focus, native_calls) = retained_parent(&root.path().join("outbox")).await;
    let workspace_path = root.path().join("workspace");
    let workspace = Workspace::open_in(&workspace_path);
    assert_eq!(workspace.connection("missing-route").unwrap_err().code, ErrorCode::NotFound);
    for changed in ["original", "native-clock", "receipt"] {
        let mut forged = focus.clone();
        match changed {
            "original" => forged.original.elapsed_ms = Some("123".into()),
            "native-clock" => forged.original_observed_at = "2020-01-01T12:00:00.000100001Z".into(),
            "receipt" => forged.parent.payload_sha256 = digest(b"a different native payload"),
            _ => unreachable!(),
        }
        assert!(forged.is_valid(), "{changed} is structurally valid but not the retained original");
        let error = workspace
            .collect_into_outbox("missing-route", request(forged), &mut outbox, &CollectionCancellation::default())
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.detail,
                Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
                    | Some(ErrorDetail::Outbox(OutboxProblem::Integrity))
            ),
            "{changed}: {error:?}"
        );
        assert_no_workspace_work(&workspace, &workspace_path);
        assert_eq!(outbox.status(10, None).await.unwrap().captures, 1, "{changed} must not reserve a capture");
        assert_eq!(native_calls.load(Ordering::SeqCst), 2);
    }
    let (source, original) = outbox.diagnostic_evidence(&focus.parent).await.unwrap();
    assert!(focus.matches_parent(
        &source,
        match &original.record {
            TelemetryRecord::Activity(row) => row,
            _ => panic!(),
        },
        &focus.parent
    ));
}

#[tokio::test]
async fn an_authenticated_focus_reaches_route_validation_before_admission_or_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (mut outbox, focus, native_calls) = retained_parent(&root.path().join("outbox")).await;
    let workspace_path = root.path().join("workspace");
    let workspace = Workspace::open_in(&workspace_path);
    let error = workspace
        .collect_into_outbox("missing-route", request(focus), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "a valid parent must reach normal route validation");
    assert_no_workspace_work(&workspace, &workspace_path);
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.unfinished), (1, 0, 0));
    assert_eq!(native_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn collect_once_refuses_focused_work_without_an_immutable_parent_resolver() {
    let root = tempfile::tempdir().unwrap();
    let (_, focus, native_calls) = retained_parent(&root.path().join("outbox")).await;
    let workspace_path = root.path().join("workspace");
    let workspace = Workspace::open_in(&workspace_path);
    let output = root.path().join("focused-output");
    let error = workspace
        .collect_once("missing-route", request(focus), &output, &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
    assert_no_workspace_work(&workspace, &workspace_path);
    assert!(!output.exists());
    assert_eq!(native_calls.load(Ordering::SeqCst), 2);
}
