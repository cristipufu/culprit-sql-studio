use super::*;
use crate::analytical::{envelope, test_support};
use culprit_core::{DatabaseIdentity, Engine};

const PRIVATE_VALUES: &[&str] = &[
    "private-investigation-database",
    "private-investigation-host.invalid",
    "private-investigation-instance",
    "private-investigation-incarnation",
    "private-investigation-client",
    "private_original_investigation_sql",
    "private_observer_cost_sql",
    "native-cost-reader-db-canary",
    "native-cost-reader-host-canary.invalid",
    "native-cost-reader-machine-canary",
    "native-cost-reader-instance-canary",
    "native-cost-reader-process-canary",
    "2020-01-01T11:30:00.0030000",
];

fn private_investigation() -> (DiagnosticInvestigationIntent, DiagnosticEpisode) {
    let mut fixture = test_support::investigation_fixture();
    let mut database = DatabaseIdentity::server(
        Engine::Mssql,
        PRIVATE_VALUES[0],
        PRIVATE_VALUES[1],
        54339,
        Some(PRIVATE_VALUES[2].into()),
    );
    database.incarnation = Some(PRIVATE_VALUES[3].into());
    fixture.baseline_source.target = database.into();
    let mut observation = fixture.intent.focus.participant().original.clone();
    observation.database = Some(PRIVATE_VALUES[0].into());
    observation.client = Some(ActivityClient { application: Some(PRIVATE_VALUES[4].into()), user: None, host: None });
    observation.statement = DefinitionPayload::sql(
        "SELECT private_original_investigation_sql FROM dbo.orders WHERE id=9264",
        Engine::Mssql,
    );
    let row = ExportedRecord::new(
        &fixture.baseline_source,
        &fixture.baseline_record.capture,
        TelemetryRecord::from(observation.clone()),
    )
    .unwrap();
    let reference = DiagnosticEvidenceRef::from_export(
        &fixture.baseline_source,
        &digest(b"private investigation baseline manifest"),
        &row,
        0,
    )
    .unwrap();
    let update = observe_diagnostic_activity(
        &fixture.baseline_source,
        &reference.capture_ref(),
        &[DiagnosticActivityRow { reference: &reference, observation: &observation }],
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    let episode = update.checkpoint.episodes[0].clone();
    fixture.intent.focus = update.focuses[0].1.clone();
    fixture.intent.episode_id = episode.id.clone();
    fixture.intent.episode_revision = episode.revision.clone();
    fixture.intent.qualification.scope = RecipeCostScope::Activity(episode.scope.clone());
    fixture.intent.qualification.read_metrics.templates[0].statement = DefinitionPayload::sql(
        "SELECT private_observer_cost_sql FROM sys.dm_exec_requests WHERE session_id=9264",
        Engine::Mssql,
    );
    fixture.intent.qualification.read_metrics.native_session_work =
        test_support::native_source_overhead_read_metrics().native_session_work;
    assert!(fixture.intent.is_valid());
    assert!(episode.matches_focus(&fixture.intent.focus));
    (fixture.intent, episode)
}

fn focused_capture(intent: &DiagnosticInvestigationIntent) -> test_support::Recovery {
    let result = ActivityWaitFocusResult {
        focus: intent.focus.page_wait().unwrap().clone(),
        observed_from: "2020-01-01T12:00:01Z".into(),
        observed_until: "2020-01-01T12:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    let source = CollectionDescription {
        partition: intent.qualification.scope.partition().clone(),
        target: intent.qualification.scope.target(),
        source: culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 0,
                sources: vec![StateSourceCoverage::observed(
                    culprit_core::source_id::PAGE_WAIT_TARGETS.into(),
                    0,
                    false,
                )],
            },
            result: Box::new(result),
        },
    };
    let mut recovery = test_support::Recovery::capture("fixture:test", &digest(b"schema"), source, vec![], 3);
    recovery.manifest.investigation = Some(DiagnosticInvestigationLineage::from_intent(intent).unwrap());
    recovery.manifest.validate().unwrap();
    recovery
}

fn expected_lineage(intent: &DiagnosticInvestigationIntent) -> Value {
    json!({
        "intent_key": intent.key().unwrap(),
        "episode_id": intent.episode_id,
        "episode_revision": intent.episode_revision,
        "recipe_id": intent.recipe_id,
        "recipe_version": intent.recipe_version,
        "budget": intent.budget,
        "component": intent.qualification.component,
        "qualification_key": intent.qualification.key().unwrap(),
        "cost_evidence": intent.qualification.evidence,
        "execution": {"kind": "database"},
    })
}

fn assert_private_values_absent(value: &Value) {
    let encoded = serde_json::to_string(value).unwrap();
    for canary in PRIVATE_VALUES {
        assert!(!encoded.contains(canary), "private value leaked into public projection: {canary}");
    }
}

#[test]
fn shared_execution_publishes_exact_owner_receipt_without_native_resource_or_question_details() {
    let fixture = test_support::shared_investigation_fixture();
    let intent = fixture.investigation.intent;
    let recovery = focused_capture(&intent);
    let public = envelope::capture_context(&recovery.manifest, &digest(b"shared-recovery-manifest")).unwrap();
    let lineage = &public["investigation"];
    let DiagnosticExecutionBinding::Shared { owner } = &intent.execution else { panic!() };
    assert_eq!(lineage["execution"]["kind"], "shared");
    assert_eq!(lineage["execution"]["owner"], serde_json::to_value(owner).unwrap());
    assert_eq!(lineage["intent_key"], intent.key().unwrap());
    let encoded = serde_json::to_string(&public).unwrap();
    assert!(!encoded.contains("private-diagnostic"));
    for private in ["target", "partition", "focus", "read_metrics", "scope"] {
        assert!(lineage.get(private).is_none(), "{private}");
    }
    let stored = recovery.manifest.investigation.as_ref().unwrap().reconstruct(&intent.focus).unwrap();
    assert_eq!(stored, intent);
    assert!(stored.matches_shared_owner(&fixture.owner_source, &fixture.owner_record));
}

#[test]
fn diagnostic_investigation_events_publish_safe_receipts_without_private_intents_or_observer_sql() {
    let (intent, episode) = private_investigation();
    let recovery = focused_capture(&intent);
    let capture = DiagnosticCaptureRef {
        capture: recovery.manifest.capture.clone(),
        manifest_sha256: recovery.manifest.digest().unwrap(),
        source_scope_id: recovery.manifest.source.captured_range().unwrap().scope_id,
    };
    let attempt = DiagnosticAttemptReceipt {
        capture: capture.capture.clone(),
        manifest_sha256: Some(capture.manifest_sha256.clone()),
    };
    for (attempt, source_work, outcome) in [
        (None, SourceWorkState::NotStarted, DiagnosticInvestigationOutcome::Admitted),
        (
            Some(attempt.clone()),
            SourceWorkState::Quiescent,
            DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::EvidenceUnavailable },
        ),
        (Some(attempt), SourceWorkState::Quiescent, DiagnosticInvestigationOutcome::Captured { capture }),
    ] {
        let event = DiagnosticInvestigationEvent { intent: Box::new(intent.clone()), attempt, source_work, outcome };
        let revision =
            DiagnosticRevision::with_investigation(&episode, episode.clone(), event.clone(), "2020-01-01T12:00:03Z")
                .unwrap()
                .unwrap();
        let record = TelemetryRecord::Operational(OperationalObservation {
            source: DIAGNOSTICS_SOURCE.into(),
            key: revision.id.clone(),
            fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
        });
        assert!(record.is_valid());
        let original = serde_json::to_value(&record).unwrap();
        let original_bytes = serde_json::to_vec(&record).unwrap();
        let private_json = serde_json::to_string(&original).unwrap();
        for canary in PRIVATE_VALUES {
            assert!(private_json.contains(canary), "the recovery fixture must contain {canary}");
        }

        let prepared = input(&record);
        let public_event = json!({
            "key": event.key().unwrap(),
            "lineage": expected_lineage(&intent),
            "attempt": event.attempt,
            "source_work": event.source_work,
            "outcome": event.outcome,
        });
        let mut expected = original.clone();
        expected["data"]["fact"]["data"]["episode"]["scope"] = json!({"key": episode.scope.key().unwrap()});
        expected["data"]["fact"]["data"]["investigation"] = public_event.clone();
        assert_eq!(prepared["record"], expected);
        assert_eq!(prepared["record"]["data"]["fact"]["data"]["investigation"], public_event);
        assert_eq!(prepared["record"]["data"]["fact"]["data"]["episode"]["investigation_event"], event.key().unwrap());
        assert_private_values_absent(&prepared);

        let projected = wire(schema::facts::operational(&[]).unwrap(), &prepared);
        let details: Value = serde_json::from_str(projected["Details"].as_str().unwrap()).unwrap();
        assert_eq!(details, expected["data"]["fact"]["data"]);
        assert_eq!(projected["DetailState"], "inline");
        assert_private_values_absent(&projected);
        assert_eq!(
            serde_json::to_vec(&record).unwrap(),
            original_bytes,
            "recovery retains the complete immutable event"
        );
    }
}

#[tokio::test]
async fn capture_envelope_keeps_admitted_lineage_without_private_scope_original_sql_or_cost_queries() {
    let (intent, _) = private_investigation();
    let recovery = focused_capture(&intent);
    let manifest = &recovery.manifest;
    let original = serde_json::to_vec(manifest).unwrap();
    let batch = &manifest.batches[0];
    assert!(batch.rows.is_empty(), "a missing requester must not gain a fabricated page row");
    envelope::Envelope::new(manifest, &batch.batch_id).unwrap();
    let context = envelope::capture_context(manifest, &manifest.digest().unwrap()).unwrap();
    assert_eq!(context["investigation"], expected_lineage(&intent));
    assert_eq!(context["focus_key"], intent.focus.page_wait().unwrap().key().unwrap());
    assert_eq!(context["range_kind"], "focused");
    assert_eq!(context["observed_from"], "2020-01-01T12:00:01Z");
    assert_eq!(context["observed_until"], "2020-01-01T12:00:02Z");
    assert_eq!(context["resource_id"], intent.focus.participant().target.telemetry_id().unwrap());
    assert_private_values_absent(&context);
    assert_eq!(serde_json::to_vec(manifest).unwrap(), original, "public projection cannot change the native manifest");
    let restored = manifest.investigation.as_ref().unwrap().reconstruct(&intent.focus).unwrap();
    assert_eq!(restored, intent);
    let range = crate::delivery::recovery::reopen(&recovery, &manifest.range_id).await.unwrap();
    let catalog = crate::analytical::catalog::PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let publication = crate::analytical::publication::input(&catalog, &range).unwrap();
    let projected = wire(schema::control::published_ranges().unwrap(), &publication);
    let lineage: Value = serde_json::from_str(projected["InvestigationContext"].as_str().unwrap()).unwrap();
    assert_eq!(
        lineage,
        expected_lineage(&intent),
        "even a successful empty capture must publish its safe owner context"
    );
    assert_private_values_absent(&projected);
}
