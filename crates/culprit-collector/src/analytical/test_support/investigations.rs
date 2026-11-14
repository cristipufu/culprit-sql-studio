//! Structural admission fixture only. Its synthetic cost artifact does not qualify any production recipe.
use super::*;
use culprit_core::{ComponentVersion, MeasurementScope, ObservedWaitResource, WaitResourceKind};

#[derive(Clone)]
pub struct InvestigationFixture {
    pub intent: DiagnosticInvestigationIntent,
    pub episode: DiagnosticEpisode,
    pub recipe: RecipeDescriptor,
    pub source: CollectionDescription,
    pub record: ExportedRecord,
    pub baseline_source: CollectionDescription,
    pub baseline_record: ExportedRecord,
}

pub fn investigation_fixture() -> InvestigationFixture {
    let (mut baseline_source, mut records) = activity_evidence_cases().remove(0);
    let TelemetryRecord::Activity(mut observation) = records.remove(0) else { unreachable!() };
    observation.waiting_on_lock = None;
    observation.elapsed_ms = Some("1".into());
    observation.wait = Some(ActivityWait {
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
        identified_resources: vec![ObservedWaitResource {
            kind: WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("7:1:4144"),
        }],
    });
    let SourceObservation::Activity(frame) = &mut baseline_source.observation else { unreachable!() };
    frame.native_rows = 1;
    frame.observations = 1;
    frame.coverage = StateSourceCoverage::collected(baseline_source.source.clone(), 1);
    let capture = fixture().0.capture;
    let baseline_record =
        ExportedRecord::new(&baseline_source, &capture, TelemetryRecord::from(observation.as_ref().clone())).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(
        &baseline_source,
        &digest(b"investigation-fixture-native-baseline-manifest"),
        &baseline_record,
        0,
    )
    .unwrap();
    let rows = [DiagnosticActivityRow { reference: &reference, observation: &observation }];
    let update = observe_diagnostic_activity(
        &baseline_source,
        &reference.capture_ref(),
        &rows,
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    assert_eq!(update.checkpoint.episodes.len(), 1);
    let episode = update.checkpoint.episodes[0].clone();
    let focus = update.focuses[0].1.clone();
    let source = CollectionDescription {
        target: baseline_source.target.clone(),
        source: "fixture-native-cost".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected("fixture-native-cost".into(), 1)],
        }),
    };
    let cost = NativeCounterSet {
        identity: NativeIdentity::new("owned-native-observer"),
        scope: MeasurementScope::Database,
        counters: vec![NativeCounterSample::gauge(
            "logical_reads",
            12,
            CounterUnit::Count,
            MeasurementScope::Database,
            "native-observer",
            "logical_reads",
        )],
        properties: Default::default(),
    };
    let record = ExportedRecord::new(
        &source,
        &CaptureIdentity {
            observation_id: "fixture-cost-observation".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-cost-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        TelemetryRecord::Operational(OperationalObservation {
            source: source.source.clone(),
            key: cost.key().unwrap(),
            fact: OperationalFact::CounterSet(Box::new(cost)),
        }),
    )
    .unwrap();
    let component = ComponentVersion::package("fixture-native-reader", "1.0.0", Some("fixture-native-reader-build"));
    let receipt = RecipeCostReceipt {
        recipe_id: "fixture.page-wait-focus".into(),
        recipe_version: 1,
        scope: episode.scope.clone(),
        component: component.clone(),
        tested_budget: CollectionBudget {
            page_rows: 1,
            max_snapshot_rows: 1,
            max_queries: 3,
            statement_timeout_ms: 1_000,
            total_timeout_ms: 5_000,
            ..Default::default()
        },
        evidence: DiagnosticEvidenceRef::from_export(
            &source,
            &digest(b"investigation-fixture-native-cost-manifest"),
            &record,
            0,
        )
        .unwrap(),
        read_metrics: ReadWorkMetrics {
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"investigation-fixture-native-query"),
                statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                completed_tasks: 2,
                failures: 0,
                timeouts: 0,
                cancellations: 0,
                unconfirmed_cancellations: 0,
                rows_decoded: "2".into(),
                charged_value_bytes: "4096".into(),
                client_elapsed_ms: 1.25,
            }],
        },
    };
    let recipe = RecipeDescriptor {
        id: receipt.recipe_id.clone(),
        version: receipt.recipe_version,
        purpose: RecipePurpose::Investigation,
        selector: RecipeSelector::FocusedPageWait,
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Measured(Box::new(receipt)),
    };
    let intent =
        DiagnosticInvestigationIntent::new(&episode, &recipe, focus, &component, &CollectionBudget::default()).unwrap();
    InvestigationFixture { intent, episode, recipe, source, record, baseline_source, baseline_record }
}
