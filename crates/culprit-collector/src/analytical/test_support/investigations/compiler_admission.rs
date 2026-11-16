//! Synthetic native identities, admission points and structural receipts. No fixture qualifies production work.
use super::*;

pub fn compiler_admission_investigation_fixture() -> InvestigationFixture {
    let mut fixture = request_memory_investigation_fixture();
    let TelemetryRecord::Activity(mut original) = fixture.baseline_record.record.clone() else {
        panic!("activity fixture")
    };
    let wait = original.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::CompilationAdmission);
    wait.native_type = Some("RESOURCE_SEMAPHORE_QUERY_COMPILE".into());
    fixture.baseline_record = ExportedRecord::new(
        &fixture.baseline_source,
        &fixture.baseline_record.capture,
        TelemetryRecord::Activity(original),
    )
    .unwrap();
    let reference = DiagnosticEvidenceRef::from_export(
        &fixture.baseline_source,
        &digest(b"synthetic-original-compiler-manifest"),
        &fixture.baseline_record,
        0,
    )
    .unwrap();
    let TelemetryRecord::Activity(original) = &fixture.baseline_record.record else { panic!("activity fixture") };
    let observed = observe_diagnostic_activity(
        &fixture.baseline_source,
        &reference.capture_ref(),
        &[DiagnosticActivityRow { reference: &reference, observation: original }],
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    fixture.episode = observed
        .checkpoint
        .episodes
        .into_iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::CompilationAdmission { .. }))
        .unwrap();
    let DiagnosticFocusPlan::Ready(focus) =
        fixture.episode.plan_focus(&fixture.baseline_source, original, &reference).unwrap()
    else {
        panic!("compiler question")
    };
    fixture.recipe.id = "fixture.compiler-admission-focus".into();
    fixture.recipe.selector = RecipeSelector::FocusedCompilationAdmission;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.recipe.cost else { panic!("synthetic approval") };
    receipt.recipe_id = fixture.recipe.id.clone();
    receipt.selector = fixture.recipe.selector;
    receipt.scope = fixture.episode.scope.clone().into();
    receipt.tested_budget.max_snapshot_rows = 64;
    receipt.tested_budget.page_rows = 64;
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

pub fn compiler_admission_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let fixture = compiler_admission_investigation_fixture();
    let focus = fixture.intent.focus.participant();
    let at = "2020-01-01T12:00:01Z";
    let waiter = CompilerWaiterObservation { observed_at: at.into(), request: focus.original.clone() };
    let mut records = vec![TelemetryRecord::Operational(OperationalObservation {
        source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
        key: waiter.key().unwrap(),
        fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
    })];
    let mut gateways = Vec::new();
    for (pool, gate, active, waiting, threshold) in
        [("2", "Big Gateway", 1, 3, "-1"), ("3", "Small Gateway", 2, 0, "9007199254740993")]
    {
        let object = "sys.dm_exec_query_optimizer_memory_gateways";
        let identity = NativeIdentity::new(gate).qualified("native_object", object).qualified("resource_pool_id", pool);
        let mut counters = [
            ("maximum-compilers", "max_count", 4),
            ("active-compilers", "active_count", active),
            ("waiting-compilers", "waiter_count", waiting),
        ]
        .into_iter()
        .map(|(key, name, value)| {
            NativeCounterSample::gauge(key, value, CounterUnit::Count, MeasurementScope::Server, object, name)
        })
        .collect::<Vec<_>>();
        if threshold != "-1" {
            counters.push(NativeCounterSample::gauge(
                "admission-threshold-bytes",
                threshold.parse().unwrap(),
                CounterUnit::Bytes,
                MeasurementScope::Server,
                object,
                "threshold",
            ));
        }
        let native = NativeCounterSet {
            identity: identity.clone(),
            scope: MeasurementScope::Server,
            counters,
            properties: [
                ("threshold".into(), threshold.into()),
                ("threshold_factor".into(), "8".into()),
                ("is_active".into(), if threshold == "-1" { "0" } else { "1" }.into()),
            ]
            .into(),
        };
        let row = OperationalObservation::from_counters(culprit_core::source_id::COMPILATION_GATEWAYS, native).unwrap();
        assert!(CompilerGatewayFacts::from_observation(&row).is_ok());
        records.push(TelemetryRecord::Operational(row));
        gateways.push(identity);
    }
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedCompilationAdmission {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CompilerAdmission,
                observed_from: at.into(),
                observed_until: at.into(),
                records: records.len() as u32,
                sources: vec![StateSourceCoverage::collected(
                    culprit_core::source_id::COMPILATION_GATEWAYS.into(),
                    records.len() as u32,
                )],
            },
            result: Box::new(ActivityCompilationAdmissionFocusResult {
                focus: focus.clone(),
                observed_from: at.into(),
                observed_until: at.into(),
                source: FocusSourceState::Matched,
                membership: CompilationAdmissionMembership::Observed,
                gateways,
            }),
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: fixture.intent.subject(),
        selection: fixture.intent.selection(),
        budget: fixture.intent.budget
    }));
    (source, records)
}
