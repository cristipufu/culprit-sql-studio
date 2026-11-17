use super::*;

#[test]
fn outcome_analysis_selects_only_operational_samples_and_does_not_enable_payload_reads() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("connection-outcomes", options);
        assert!(args.analysis == Analysis::ConnectionOutcomes);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
}

#[test]
fn query_population_and_activity_range_options_cannot_be_borrowed_by_outcome_analysis() {
    for options in [
        vec!["--workload-population", "7"],
        vec!["--activity-range", "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"],
    ] {
        let args = analyze_args("connection-outcomes", &options);
        assert!(args.validate_selection().is_err());
    }
}
