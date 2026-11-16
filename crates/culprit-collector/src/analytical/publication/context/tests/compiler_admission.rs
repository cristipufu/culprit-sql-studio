use super::*;

#[tokio::test]
async fn compiler_publication_preserves_coverage_and_safe_approval_without_original_scope_or_sql() {
    let mut fixture = test_support::compiler_admission_investigation_fixture();
    fixture.intent.qualification.read_metrics.templates[0].statement = DefinitionPayload::sql(
        "SELECT private_compiler_cost_query FROM private_compiler_cost_observer WHERE label='private-cost-label'",
        Engine::Mssql,
    );
    assert!(fixture.intent.is_valid());
    let (source, records) = test_support::compiler_admission_evidence();
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let mut recovery =
        test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records, 240_001);
    recovery.manifest.investigation = Some(DiagnosticInvestigationLineage::from_intent(&fixture.intent).unwrap());
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let published = super::super::super::input(&catalog, &range).unwrap();
    let public = &published["source"]["observation"];
    let SourceObservation::FocusedCompilationAdmission { snapshot, result } = &source.observation else { panic!() };
    assert_eq!(public["kind"], "focused-compilation-admission-context");
    assert_eq!(public["data"]["snapshot"], serde_json::to_value(snapshot).unwrap());
    assert_eq!(public["data"]["focus_key"], result.focus.key().unwrap());
    assert_eq!(public["data"]["membership"], "observed");
    assert_eq!(public["data"]["source"], "matched");
    assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
    for private in ["focus", "original", "target", "partition", "gateways"] {
        assert!(public["data"].get(private).is_none(), "{private}");
    }
    let control = &published["_culprit"];
    assert_eq!(control["focus_key"], result.focus.key().unwrap());
    assert_eq!(control["range_kind"], "focused");
    let owner = &control["investigation"];
    assert_eq!(owner["intent_key"], fixture.intent.key().unwrap());
    assert_eq!(owner["qualification_key"], fixture.intent.qualification.key().unwrap());
    assert_eq!(owner["cost_evidence"], serde_json::to_value(&fixture.intent.qualification.evidence).unwrap());
    assert_eq!(owner["budget"], serde_json::to_value(&fixture.intent.budget).unwrap());
    assert_eq!(owner["execution"]["kind"], "database");
    for private in ["scope", "target", "partition", "qualification", "read_metrics"] {
        assert!(owner.get(private).is_none(), "{private}");
    }
    let encoded = serde_json::to_string(&published).unwrap();
    for private in [
        "private-original-memory-native-source",
        "private-memory-native-member",
        "private-memory-native-namespace",
        "private-memory-native-epoch",
        "private-memory-client",
        "private-memory-login",
        "private-memory-host",
        "private_memory_original",
        "private_compiler_cost",
    ] {
        assert!(!encoded.contains(private), "{private}");
    }
}

#[tokio::test]
async fn another_valid_compiler_question_cannot_reuse_the_sealed_investigation_owner() {
    let fixture = test_support::compiler_admission_investigation_fixture();
    let (source, records) = test_support::compiler_admission_evidence();
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let mut recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records, 240_001);
    recovery.manifest.investigation = Some(DiagnosticInvestigationLineage::from_intent(&fixture.intent).unwrap());
    let SourceObservation::FocusedCompilationAdmission { result, .. } = &mut recovery.manifest.source.observation
    else {
        panic!()
    };
    result.focus.parent.payload_sha256 = digest(b"another valid compiler admission parent payload");
    assert!(result.is_valid());
    let error = reopen(&recovery, &recovery.manifest.range_id).await.err().unwrap();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
