use super::*;

#[tokio::test]
async fn memory_focus_publication_retains_only_its_key_state_coverage_and_safe_immutable_owner_summary() {
    let fixture = test_support::request_memory_investigation_fixture();
    let (source, records) = test_support::focused_request_memory_evidence();
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let mut recovery =
        test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records, 240_000);
    recovery.manifest.investigation = Some(DiagnosticInvestigationLineage::from_intent(&fixture.intent).unwrap());
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let published = super::super::super::input(&catalog, &range).unwrap();
    let public = &published["source"]["observation"];
    assert_eq!(public["kind"], "focused-request-memory-context");
    let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
    assert_eq!(public["data"]["focus_key"], result.focus.key().unwrap());
    assert_eq!(public["data"]["membership"], "observed");
    assert_eq!(public["data"]["source"], "matched");
    assert!(public["data"].get("focus").is_none());
    assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
    assert_eq!(published["_culprit"]["focus_key"], result.focus.key().unwrap());
    assert_eq!(published["_culprit"]["investigation"]["intent_key"], fixture.intent.key().unwrap());
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
    ] {
        assert!(!encoded.contains(private), "{private}");
    }
}

#[tokio::test]
async fn a_different_structurally_valid_memory_question_cannot_borrow_the_sealed_owner_envelope() {
    let fixture = test_support::request_memory_investigation_fixture();
    let (source, records) = test_support::focused_request_memory_evidence();
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let mut recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records, 240_000);
    recovery.manifest.investigation = Some(DiagnosticInvestigationLineage::from_intent(&fixture.intent).unwrap());
    let SourceObservation::FocusedRequestMemory { result, .. } = &mut recovery.manifest.source.observation else {
        panic!()
    };
    result.focus.parent.payload_sha256 = digest(b"another valid original payload digest");
    assert!(result.is_valid());
    let error = reopen(&recovery, &recovery.manifest.range_id).await.err().unwrap();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
