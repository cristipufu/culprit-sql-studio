//! Synthetic native points and structural receipts test transport; no fixture grants production work authority.
use super::*;

pub fn request_memory_investigation_fixture() -> InvestigationFixture {
    let mut fixture = investigation_fixture();
    let (native, _) =
        request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }, true);
    fixture.baseline_source.partition = native.partition;
    let SourcePartition::InstanceLocal { member: Some(member), epoch } = &mut fixture.baseline_source.partition else {
        unreachable!()
    };
    member.native.id = "private-memory-native-member".into();
    member.namespace.id = "private-memory-native-namespace".into();
    *epoch = Some("private-memory-native-epoch".into());
    let epoch = fixture.baseline_source.partition.native_epoch().unwrap().to_owned();
    fixture.baseline_source.source = "private-original-memory-native-source".into();
    let SourceObservation::Activity(frame) = &mut fixture.baseline_source.observation else { unreachable!() };
    frame.epoch = Some(epoch);
    frame.database_writable = Some(true);
    frame.coverage.source = fixture.baseline_source.source.clone();
    let TelemetryRecord::Activity(mut original) = fixture.baseline_record.record.clone() else { unreachable!() };
    original.blocking_status = ActivityBlockingStatus::Unknown;
    original.elapsed_ms = Some("1".into());
    original.statement = DefinitionPayload::sql(
        "SELECT private_memory_original_column FROM dbo.private_memory_original_table WHERE id=9264",
        fixture.baseline_source.target.engine(),
    );
    original.client = Some(ActivityClient {
        application: Some("private-memory-client-application".into()),
        user: Some("private-memory-login".into()),
        host: Some("private-memory-host".into()),
    });
    let wait = original.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::ExecutionGrant);
    wait.native_type = Some("RESOURCE_SEMAPHORE".into());
    wait.identified_resources.clear();
    wait.owners.clear();
    fixture.baseline_record = ExportedRecord::new(
        &fixture.baseline_source,
        &fixture.baseline_record.capture,
        TelemetryRecord::Activity(original),
    )
    .unwrap();
    let reference = DiagnosticEvidenceRef::from_export(
        &fixture.baseline_source,
        &digest(b"request-memory-original-native-manifest"),
        &fixture.baseline_record,
        0,
    )
    .unwrap();
    let TelemetryRecord::Activity(original) = &fixture.baseline_record.record else { unreachable!() };
    let update = observe_diagnostic_activity(
        &fixture.baseline_source,
        &reference.capture_ref(),
        &[DiagnosticActivityRow { reference: &reference, observation: original }],
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    fixture.episode = update
        .checkpoint
        .episodes
        .iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::RequestMemory { .. }))
        .unwrap()
        .clone();
    let DiagnosticFocusPlan::Ready(focus) =
        fixture.episode.plan_focus(&fixture.baseline_source, original, &reference).unwrap()
    else {
        unreachable!()
    };
    fixture.recipe.id = "fixture.request-memory-focus".into();
    fixture.recipe.selector = RecipeSelector::FocusedRequestMemory;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.recipe.cost else { unreachable!() };
    receipt.recipe_id = fixture.recipe.id.clone();
    receipt.selector = fixture.recipe.selector;
    receipt.scope = fixture.episode.scope.clone().into();
    let component = receipt.component.clone();
    fixture.intent = DiagnosticInvestigationIntent::new(
        &fixture.episode,
        &fixture.recipe,
        *focus,
        &component,
        &CollectionBudget::default(),
    )
    .unwrap();
    fixture
}

pub fn focused_request_memory_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let fixture = request_memory_investigation_fixture();
    let focus = fixture.intent.focus.participant();
    let (_, mut rows) =
        request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }, true);
    let TelemetryRecord::Operational(row) = &mut rows[0] else { unreachable!() };
    let OperationalFact::ExecutionGrant(grant) = &mut row.fact else { unreachable!() };
    let at = "2020-01-01T12:00:01Z".to_owned();
    grant.identity = NativeIdentity::new("fixture-request-memory-grant")
        .qualified("request-generation", &focus.original.key.request.as_ref().unwrap().generation);
    grant.observed_at = at.clone();
    grant.state = ExecutionGrantState::Waiting;
    grant.granted_without_offset = None;
    grant.granted_kibibytes = None;
    grant.wait_ms = Some("40000".into());
    grant.native_null_columns.retain(|column| column != "wait_time_ms");
    grant.native_null_columns.extend(["grant_time".into(), "granted_memory_kb".into()]);
    grant.request = Some(focus.original.clone());
    grant.statement_relationship = GrantStatementRelationship::ObservedCurrentStatement;
    row.key = grant.key().unwrap();
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: grant.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedRequestMemory {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestMemory,
                observed_from: at.clone(),
                observed_until: at.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(grant.source.clone(), 1, true)],
            },
            result: Box::new(ActivityRequestMemoryFocusResult {
                focus: focus.clone(),
                observed_from: at.clone(),
                observed_until: at,
                source: FocusSourceState::Matched,
                membership: RequestMemoryMembership::Observed,
            }),
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: fixture.intent.subject(),
        selection: fixture.intent.selection(),
        budget: fixture.intent.budget
    }));
    (source, rows)
}
