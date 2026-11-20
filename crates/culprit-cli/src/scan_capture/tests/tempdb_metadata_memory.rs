use super::*;

#[test]
fn metadata_attachment_requires_the_existing_explicit_outbox_and_excludes_other_captured_families() {
    assert!(args(&["--tempdb-metadata-memory-capture", "metadata-observation"]).is_err());
    let parsed =
        args(&["--evidence-outbox", "owned-outbox", "--tempdb-metadata-memory-capture", "metadata-observation"])
            .unwrap();
    let original = parsed.captured.original().unwrap().unwrap();
    assert_eq!(original.observation_id, "metadata-observation");
    assert_eq!(original.analysis, LocalCaptureAnalysisKind::TempdbMetadataMemory);
    assert!(!parsed.snapshot_options().collect_shared_resources, "saved evidence is not collection permission");
    for other in ["--memory-clerks-capture", "--hyperscale-log-components-capture"] {
        assert!(
            args(&[
                "--evidence-outbox",
                "owned-outbox",
                "--tempdb-metadata-memory-capture",
                "metadata-observation",
                other,
                "other-observation"
            ])
            .is_err()
        );
    }
    assert!(args(&[]).unwrap().captured.original().unwrap().is_none());
}
