use super::*;

#[tokio::test]
async fn focused_native_activity_publishes_its_boundary_and_opaque_parent_key_without_copying_original_context() {
    for (source, records) in test_support::blocking_evidence_cases() {
        let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records, 240_020);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let published = super::super::super::input(&catalog, &range).unwrap();
        let public = &published["source"]["observation"];
        let SourceObservation::FocusedBlocking { snapshot, result } = &source.observation else { unreachable!() };
        assert_eq!(public["kind"], "focused-blocking-context");
        assert_eq!(public["data"]["focus_key"], result.focus.key().unwrap());
        assert_eq!(public["data"]["snapshot"], serde_json::to_value(snapshot).unwrap());
        for (key, value) in [
            ("binding", serde_json::to_value(result.binding).unwrap()),
            ("membership", serde_json::to_value(result.membership).unwrap()),
            ("request_end", serde_json::to_value(result.request_end).unwrap()),
            ("closure", serde_json::to_value(&result.closure).unwrap()),
        ] {
            assert_eq!(public["data"][key], value, "{key}");
        }
        assert_eq!(published["_culprit"]["focus_key"], result.focus.key().unwrap());
        assert_eq!(published["_culprit"]["range_kind"], "focused");
        assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
        for private in ["focus", "original", "target", "partition", "statement", "parent"] {
            assert!(public["data"].get(private).is_none(), "{private}");
        }
        let encoded = serde_json::to_string(&published).unwrap();
        for private in [
            "private-blocking-native-member",
            "private-blocking-native-lineage",
            "SELECT id",
            "VACUUM orders",
            "fixture.invalid",
        ] {
            assert!(!encoded.contains(private), "{private}");
        }
    }
}
