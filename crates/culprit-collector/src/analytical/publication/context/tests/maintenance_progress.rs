use super::*;

#[tokio::test]
async fn progress_context_exposes_native_coverage_and_binding_without_republishing_original_sql_or_owner() {
    for (source, records) in test_support::maintenance_progress_evidence_cases() {
        let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records, 240_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let published = super::super::super::input(&catalog, &range).unwrap();
        let public = &published["source"]["observation"];
        let SourceObservation::MaintenanceProgress { result, snapshot } = &source.observation else { panic!() };
        assert_eq!(public["kind"], "maintenance-progress-context");
        let focus_key = result.focus.as_ref().unwrap().key().unwrap();
        assert_eq!(public["data"]["focus_key"], focus_key);
        assert_eq!(public["data"]["snapshot"], serde_json::to_value(snapshot).unwrap());
        assert_eq!(public["data"]["membership"], serde_json::to_value(result.membership).unwrap());
        assert_eq!(public["data"]["binding"], serde_json::to_value(result.binding).unwrap());
        assert_eq!(public["data"]["database_writable"], serde_json::to_value(result.database_writable).unwrap());
        assert_eq!(published["_culprit"]["focus_key"], focus_key);
        assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
        let encoded = serde_json::to_string(&published).unwrap();
        for private in ["private-maintenance", "private_maintenance", "collector-name", "fixture.invalid"] {
            assert!(!encoded.contains(private), "{private}");
        }
        for private in ["focus", "original", "actor", "statement", "target", "partition"] {
            assert!(public["data"].get(private).is_none(), "{private}");
        }
    }
}
