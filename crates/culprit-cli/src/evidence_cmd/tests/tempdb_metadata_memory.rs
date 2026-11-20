use super::*;

#[test]
fn tempdb_metadata_analysis_uses_only_original_operational_rows_without_implicit_payload_or_companion_reads() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("tempdb-metadata-memory", options);
        assert!(args.analysis == Analysis::TempdbMetadataMemory);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
    for selector in
        ["--activity-range", "--definitions-range", "--compilation-baseline", "--cache-baseline", "--blocking-range"]
    {
        let args = analyze_args("tempdb-metadata-memory", &[selector, &"a".repeat(64)]);
        assert!(args.validate_selection().is_err(), "{selector}");
    }
}
