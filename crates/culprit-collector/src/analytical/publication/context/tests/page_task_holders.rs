use super::*;

#[test]
fn page_population_context_retains_its_role_and_only_an_opaque_original_activity_association() {
    let fixture = test_support::page_task_holder_fixture(test_support::PageTaskHolderCase::QualifiedBoth);
    let SourceObservation::PageTaskHolders { focus, .. } = &fixture.source.observation else { panic!() };
    let parent = focus.participant.parent.capture_ref();
    let mut source = fixture.page_source;
    for original in [None, Some(parent.clone())] {
        let SourceObservation::PageWaitTargets { original_activity, .. } = &mut source.observation else { panic!() };
        *original_activity = original.clone();
        let public = observation(&source.observation).unwrap();
        assert_eq!(public["kind"], "page-wait-targets-context");
        assert_eq!(public["data"]["database_writable"], true);
        assert_eq!(
            public["data"]["original_activity_key"],
            serde_json::to_value(original.as_ref().map(DiagnosticCaptureRef::key).transpose().unwrap()).unwrap()
        );
        assert!(public["data"].get("original_activity").is_none());
        let encoded = serde_json::to_string(&public).unwrap();
        assert!(!encoded.contains(&parent.capture.observation_id));
        assert!(!encoded.contains("manifest_sha256"));
        assert!(serde_json::from_value::<SourceObservation>(public).is_err());
    }
}
