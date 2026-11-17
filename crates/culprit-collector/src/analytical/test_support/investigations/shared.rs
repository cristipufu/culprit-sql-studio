//! The original database generation and the shared execution generation deliberately differ.
use super::*;
use culprit_core::Engine;

pub struct SharedInvestigationFixture {
    pub investigation: InvestigationFixture,
    pub owner_source: CollectionDescription,
    pub owner_record: ExportedRecord,
}

pub fn shared_investigation_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let fixture = shared_investigation_fixture().investigation;
    let parent = fixture.intent.focus.parent().capture_ref();
    let mut next = fixture.episode.clone();
    next.analysis = DiagnosticAnalysisStatus::Collecting;
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(fixture.intent),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    };
    let at = "2020-01-01T12:00:02Z";
    let revision = DiagnosticRevision::with_investigation(&fixture.episode, next, event, at).unwrap().unwrap();
    let mut part = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: fixture.baseline_source.target.clone(),
        input: parent.clone(),
        part: 0,
        parts: 1,
        episodes: vec![revision.episode.clone()],
    };
    part.id = part.expected_id().unwrap();
    let source = CollectionDescription {
        target: fixture.baseline_source.target,
        partition: SourcePartition::Target,
        source: DIAGNOSTICS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
            parent,
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            step_id: digest(b"shared-investigation-admission-step"),
            analyzed_at: at.into(),
            records: 1,
            checkpoint_records: 1,
            scope_bindings: [(revision.episode.scope.key().unwrap(), revision.episode.scope.clone())].into(),
        }),
    };
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: DIAGNOSTICS_SOURCE.into(),
        key: revision.id.clone(),
        fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
    });
    assert!(part.is_valid() && row.is_valid());
    (source, vec![row, TelemetryRecord::DiagnosticCheckpoint(part)])
}

pub fn shared_investigation_fixture() -> SharedInvestigationFixture {
    let mut fixture = investigation_fixture();
    let resource = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-diagnostic-shared-namespace"),
        native: NativeIdentity::new("private-diagnostic-shared-instance"),
    };
    let TelemetryRecord::Activity(original) = &mut fixture.baseline_record.record else { unreachable!() };
    original.wait.as_mut().unwrap().identified_resources[0].native = NativeIdentity::new("2:1:4144");
    let epoch = fixture.baseline_source.observation.activity().unwrap().epoch.clone();
    fixture.baseline_source.partition = SourcePartition::InstanceLocal { member: Some(resource.clone()), epoch };
    fixture.baseline_record = ExportedRecord::new(
        &fixture.baseline_source,
        &fixture.baseline_record.capture,
        fixture.baseline_record.record.clone(),
    )
    .unwrap();
    let parent = DiagnosticEvidenceRef::from_export(
        &fixture.baseline_source,
        &digest(b"shared-investigation-original-db-manifest"),
        &fixture.baseline_record,
        0,
    )
    .unwrap();
    let TelemetryRecord::Activity(original) = &fixture.baseline_record.record else { unreachable!() };
    let observed = observe_diagnostic_activity(
        &fixture.baseline_source,
        &parent.capture_ref(),
        &[DiagnosticActivityRow { reference: &parent, observation: original }],
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    fixture.episode = observed.checkpoint.episodes[0].clone();
    let focus = observed.focuses[0].1.clone();
    let owner_source = owner_source(resource.clone());
    let owner_record = counter_record(&owner_source, "fixture-shared-owner", "fixture-owner-counter");
    let owner = DiagnosticEvidenceRef::from_export(
        &owner_source,
        &digest(b"shared-investigation-owner-manifest"),
        &owner_record,
        0,
    )
    .unwrap();
    fixture.source.target = owner_source.target.clone();
    fixture.source.partition = owner_source.partition.clone();
    fixture.record = counter_record(&fixture.source, "fixture-shared-cost", "fixture-cost-counter");
    let RecipeCostQualification::Measured(receipt) = &mut fixture.recipe.cost else { unreachable!() };
    receipt.scope = RecipeCostScope::SharedResource {
        target: resource,
        partition: owner_source.partition.clone(),
        source: owner_source.source.clone(),
    };
    receipt.evidence = DiagnosticEvidenceRef::from_export(
        &fixture.source,
        &digest(b"shared-investigation-independent-cost-manifest"),
        &fixture.record,
        0,
    )
    .unwrap();
    let component = receipt.component.clone();
    fixture.recipe.subjects = vec![RecipeSubject::Shared(SharedResourceKind::Instance)];
    fixture.intent = DiagnosticInvestigationIntent::new_shared(
        &fixture.episode,
        &fixture.recipe,
        focus,
        &component,
        &CollectionBudget::default(),
        DiagnosticSharedOwner { source: &owner_source, row: &owner_record, reference: &owner },
    )
    .unwrap();
    SharedInvestigationFixture { investigation: fixture, owner_source, owner_record }
}

fn owner_source(resource: SharedResourceIdentity) -> CollectionDescription {
    CollectionDescription {
        partition: SourcePartition::InstanceLocal {
            member: Some(resource.clone()),
            epoch: Some("private-diagnostic-shared-process-generation".into()),
        },
        target: CollectionTarget::Shared(resource),
        source: "fixture-shared-operational".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected("fixture-shared-operational".into(), 1)],
        }),
    }
}

fn counter_record(source: &CollectionDescription, observation_id: &str, native: &str) -> ExportedRecord {
    let counter = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new(native),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "logical_reads",
            12,
            CounterUnit::Count,
            MeasurementScope::Server,
            "fixture-native-observer",
            "logical_reads",
        )],
        properties: Default::default(),
    };
    ExportedRecord::new(
        source,
        &CaptureIdentity {
            observation_id: observation_id.into(),
            revision: Some(CaptureRevision {
                outbox_id: "fixture-owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        TelemetryRecord::Operational(OperationalObservation {
            source: source.source.clone(),
            key: counter.key().unwrap(),
            fact: OperationalFact::CounterSet(Box::new(counter)),
        }),
    )
    .unwrap()
}
