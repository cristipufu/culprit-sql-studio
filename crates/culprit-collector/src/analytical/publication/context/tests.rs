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
        parent,
        target: baseline.target.clone(),
        source: baseline.source.clone(),
        epoch,
        original_observed_at: at,
        statement_identity: original.statement_identity.clone().unwrap(),
        original,
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
    assert_eq!(public["data"]["result"]["focus"]["parent"], serde_json::to_value(&result.focus.parent).unwrap());
    assert_eq!(
        public["data"]["result"]["focus"]["statement_identity"],
        serde_json::to_value(&result.focus.statement_identity).unwrap()
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
        controller_version: 1,
        parent,
        step_id: digest(b"step"),
        analyzed_at: "2020-01-01T12:30:00Z".into(),
        records: 0,
        checkpoint_records: 1,
    };
    assert!(description.is_valid());
    manifest.source.observation = SourceObservation::Diagnostics(description.clone());
    let public = observation(&manifest.source.observation).unwrap();
    assert_eq!(public["kind"], "diagnostic-context");
    assert_eq!(public["data"], serde_json::to_value(&description).unwrap());
    // Context projection is pure; no native records or committed range are manufactured for this test.
    let context = envelope::capture_context(&manifest, &digest(b"recovery manifest")).unwrap();
    assert_eq!(context["range_kind"], "focused");
    assert_eq!(context["diagnostic"], serde_json::to_value(description).unwrap());
    for field in ["observed_at", "observed_from", "observed_until", "interval_from", "interval_until"] {
        assert!(context.get(field).is_none(), "{field}");
    }
}

#[test]
fn automatic_lineage_preserves_approval_identity_without_its_private_scope_or_sql() {
    let mut source = focused_source();
    let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { panic!() };
    let mut intent = test_support::investigation_fixture().intent;
    intent.focus = result.focus.clone();
    intent.qualification.scope.target = intent.focus.target.clone();
    intent.qualification.scope.source = intent.focus.source.clone();
    intent.qualification.scope.native_epoch = Some(intent.focus.epoch.clone());
    intent.qualification.read_metrics.templates[0].statement = DefinitionPayload::sql(
        "SELECT private_qualification_query FROM private_cost_observer WHERE label='private-client'",
        Engine::Mssql,
    );
    assert!(intent.is_valid());
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut manifest = test_support::fixture().0;
    manifest.source = source.clone();
    manifest.investigation = Some(owner);
    let public = envelope::capture_context(&manifest, &digest(b"owner recovery manifest")).unwrap();
    let lineage = &public["investigation"];
    assert_eq!(lineage["intent_key"], intent.key().unwrap());
    assert_eq!(lineage["recipe_id"], intent.recipe_id);
    assert_eq!(lineage["qualification_key"], intent.qualification.key().unwrap());
    assert_eq!(lineage["cost_evidence"], serde_json::to_value(intent.qualification.evidence).unwrap());
    assert_eq!(lineage["budget"], serde_json::to_value(intent.budget).unwrap());
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
    ] {
        assert!(!json.contains(canary), "{canary}");
    }
}
