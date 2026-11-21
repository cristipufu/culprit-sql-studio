mod blocking;
mod compiler_admission;
mod maintenance_progress;
mod memory;
mod page_task_holders;
mod source_read;
use super::*;
use crate::{
    analytical::{catalog::PhysicalCatalog, envelope, test_support},
    delivery::recovery::reopen,
};
use culprit_core::{Engine, WaitResourceKind};

fn focused_source() -> CollectionDescription {
    let mut baseline = test_support::activity_evidence_cases().remove(0).0;
    baseline.target = culprit_core::DatabaseIdentity::server(
        Engine::Mssql,
        "private-target-db",
        "private-endpoint.invalid",
        1433,
        Some("private-native-guid".into()),
    )
    .into();
    let mut original = test_support::activity_observation(DefinitionPayload::sql(
        "SELECT private_original_query FROM dbo.orders WHERE id=9264",
        Engine::Mssql,
    ));
    original.database = Some("private-target-db".into());
    original.client = Some(ActivityClient {
        application: Some("private-client-app".into()),
        user: Some("private-client-user".into()),
        host: Some("private-client-host".into()),
    });
    let resource = ObservedWaitResource { kind: WaitResourceKind::MemoryPage, native: NativeIdentity::new("7:1:4144") };
    original.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![resource.clone()],
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
    });
    let snapshot = baseline.observation.activity().unwrap();
    let at = snapshot.at.clone();
    let epoch = snapshot.epoch.clone().unwrap();
    let capture = test_support::fixture().0.capture;
    let row = ExportedRecord::new(&baseline, &capture, TelemetryRecord::from(original.clone())).unwrap();
    let parent = DiagnosticEvidenceRef::from_export(&baseline, &digest(b"original private manifest"), &row, 0).unwrap();
    let focus = ActivityWaitFocus {
        participant: ActivityRequestFocus {
            database_writable: None,
            partition: culprit_core::telemetry::SourcePartition::Target,
            parent,
            target: baseline.target.clone(),
            source: baseline.source.clone(),
            epoch,
            original_observed_at: at,
            statement_identity: original.statement_identity.clone().unwrap(),
            original,
        },
        resource,
    };
    let result = ActivityWaitFocusResult {
        focus,
        observed_from: "2020-01-01T12:00:01Z".into(),
        observed_until: "2020-01-01T12:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    assert!(result.is_valid());
    CollectionDescription {
        partition: culprit_core::telemetry::SourcePartition::Target,
        target: baseline.target,
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
    }
}

#[tokio::test]
async fn public_focus_context_keeps_exact_work_identity_without_private_target_or_original_payload() {
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let source = focused_source();
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), vec![], 240_000);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let published = super::super::input(&catalog, &range).unwrap();
    let public = &published["source"]["observation"];
    assert_eq!(public["kind"], "focused-page-wait-context");
    assert!(
        serde_json::from_value::<SourceObservation>(public.clone()).is_err(),
        "public projection is a separate declared shape"
    );
    let SourceObservation::FocusedPageWait { result, .. } = &source.observation else { panic!() };
    assert_eq!(public["data"]["result"]["focus"]["key"], result.focus.key().unwrap());
    assert_eq!(
        public["data"]["result"]["focus"]["parent"],
        serde_json::to_value(&result.focus.participant().parent).unwrap()
    );
    assert_eq!(
        public["data"]["result"]["focus"]["statement_identity"],
        serde_json::to_value(&result.focus.participant().statement_identity).unwrap()
    );
    assert_eq!(public["data"]["result"]["request"], "missing");
    assert_eq!(published["_culprit"]["range_kind"], "focused");
    assert_eq!(published["_culprit"]["focus_key"], result.focus.key().unwrap());
    let json = serde_json::to_string(&published).unwrap();
    for canary in [
        "private-target-db",
        "private-endpoint.invalid",
        "private-native-guid",
        "private-client",
        "private_original_query",
    ] {
        assert!(!json.contains(canary), "{canary}");
    }
    assert!(public["data"]["result"]["focus"].get("target").is_none());
    assert!(public["data"]["result"]["focus"].get("original").is_none());
}

#[test]
fn diagnostic_analysis_time_remains_typed_context_and_never_becomes_a_native_observation_clock() {
    let (mut manifest, _) = test_support::fixture();
    let parent = DiagnosticCaptureRef {
        capture: manifest.capture.clone(),
        manifest_sha256: digest(b"native manifest"),
        source_scope_id: manifest.source.captured_range().unwrap().scope_id,
    };
    let description = DiagnosticCaptureDescription {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        parent,
        step_id: digest(b"step"),
        analyzed_at: "2020-01-01T12:30:00Z".into(),
        records: 0,
        checkpoint_records: 1,
        scope_bindings: Default::default(),
    };
    assert!(description.is_valid());
    manifest.source.observation = SourceObservation::Diagnostics(description.clone());
    let public = observation(&manifest.source.observation).unwrap();
    assert_eq!(public["kind"], "diagnostic-context");
    let mut expected = serde_json::to_value(&description).unwrap();
    expected.as_object_mut().unwrap().remove("scope_bindings");
    expected["scope_keys"] = serde_json::json!([]);
    assert_eq!(public["data"], expected);
    // Context projection is pure; no native records or committed range are manufactured for this test.
    let context = envelope::capture_context(&manifest, &digest(b"recovery manifest")).unwrap();
    assert_eq!(context["range_kind"], "focused");
    assert_eq!(context["diagnostic"], expected);
    for field in ["observed_at", "observed_from", "observed_until", "interval_from", "interval_until"] {
        assert!(context.get(field).is_none(), "{field}");
    }
}

#[test]
fn automatic_lineage_preserves_approval_identity_without_its_private_scope_or_sql() {
    let mut source = focused_source();
    let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { panic!() };
    let mut intent = test_support::investigation_fixture().intent;
    intent.focus = result.focus.clone().into();
    let RecipeCostScope::Activity(scope) = &mut intent.qualification.scope else { panic!() };
    scope.target = intent.focus.target().clone();
    scope.source = intent.focus.activity_participant().unwrap().source.clone();
    scope.native_epoch = Some(intent.focus.activity_participant().unwrap().epoch.clone());
    intent.qualification.read_metrics.templates[0].statement = DefinitionPayload::sql(
        "SELECT private_qualification_query FROM private_cost_observer WHERE label='private-client'",
        Engine::Mssql,
    );
    intent.qualification.read_metrics.native_session_work =
        test_support::native_source_overhead_read_metrics().native_session_work;
    assert!(intent.is_valid());
    let original = serde_json::to_vec(&intent).unwrap();
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut manifest = test_support::fixture().0;
    manifest.source = source.clone();
    manifest.investigation = Some(owner);
    let public = envelope::capture_context(&manifest, &digest(b"owner recovery manifest")).unwrap();
    let lineage = &public["investigation"];
    assert_eq!(lineage["intent_key"], intent.key().unwrap());
    assert_eq!(lineage["recipe_id"], intent.recipe_id);
    assert_eq!(lineage["qualification_key"], intent.qualification.key().unwrap());
    assert_eq!(lineage["cost_evidence"], serde_json::to_value(&intent.qualification.evidence).unwrap());
    assert_eq!(lineage["budget"], serde_json::to_value(&intent.budget).unwrap());
    for omitted in ["scope", "target", "qualification", "read_metrics"] {
        assert!(lineage.get(omitted).is_none(), "{omitted}");
    }
    let json = serde_json::to_string(&public).unwrap();
    for canary in [
        "private-target-db",
        "private-endpoint.invalid",
        "private-native-guid",
        "private-client",
        "private_original_query",
        "private_qualification_query",
        "native-cost-reader-db-canary",
        "native-cost-reader-host-canary.invalid",
        "native-cost-reader-machine-canary",
        "native-cost-reader-instance-canary",
        "native-cost-reader-process-canary",
        "2020-01-01T11:30:00.0030000",
    ] {
        assert!(!json.contains(canary), "{canary}");
    }
    assert!(!json.contains("native_session_work"));
    assert_eq!(
        serde_json::to_vec(&intent).unwrap(),
        original,
        "public lineage cannot mask or re-sign canonical authorization"
    );
}

#[tokio::test]
async fn native_source_work_publication_and_capture_context_never_embed_private_reader_authority() {
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let (source, rows) = test_support::memory_failure_evidence();
    let mut recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, rows, 240_001);
    let metrics = test_support::native_source_overhead_read_metrics();
    recovery.manifest.source_queries = test_support::SOURCE_OVERHEAD_QUERY_COUNT;
    recovery.manifest.read_metrics = Some(metrics.clone());
    recovery.manifest.validate().unwrap();
    let original = serde_json::to_vec(&recovery.manifest).unwrap();
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let published = super::super::input(&catalog, &range).unwrap();
    test_support::assert_native_source_overhead_summary(&published["read_metrics"], &metrics);
    assert_eq!(published["source_queries"], test_support::SOURCE_OVERHEAD_QUERY_COUNT);
    assert_eq!(
        published["read_metrics"],
        serde_json::to_value(metrics.summary(Some(&recovery.manifest.source)).unwrap()).unwrap()
    );
    assert!(
        published["_culprit"].get("read_metrics").is_none(),
        "one public numeric projection, no embedded private copy"
    );
    assert_eq!(
        serde_json::to_vec(&recovery.manifest).unwrap(),
        original,
        "the native proof and manifest digest remain immutable"
    );
    assert!(String::from_utf8(original).unwrap().contains("native-cost-reader-process-canary"));
}

#[test]
fn managed_instance_history_publication_keeps_native_availability_without_current_reader_authority() {
    let (source, rows) = test_support::managed_instance_resource_evidence();
    let public = observation(&source.observation).unwrap();
    assert_eq!(public["kind"], "mssql-managed-instance-resources");
    assert_eq!(public["data"]["native_engine_edition"], 8);
    assert_eq!(public["data"]["layout"]["hardware_generation"], "unsupported");
    let encoded = serde_json::to_string(&public).unwrap();
    for private in ["current_reader", "private-current-machine", "private-current-instance", "private-current-process"]
    {
        assert!(!encoded.contains(private), "{private}");
    }
    let recovery = test_support::Recovery::capture("fixture:mi", &digest(b"mi schema"), source, rows, 2);
    let context = envelope::capture_context(&recovery.manifest, &digest(b"mi original manifest")).unwrap();
    assert_eq!(context["observed_at"], "2020-01-01T06:10:00Z");
    assert_eq!(context["interval_from"], "2020-01-01T06:00:00.000000000Z");
}
