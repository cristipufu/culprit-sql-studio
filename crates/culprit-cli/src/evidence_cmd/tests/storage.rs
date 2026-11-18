use super::*;

#[test]
fn storage_selects_only_original_operational_rows_without_queries_or_payload_hydration() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("storage", options);
        assert!(args.analysis == Analysis::Storage);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
    assert!(analyze_args("storage", &["--workload-population", "1"]).validate_selection().is_err());
}
