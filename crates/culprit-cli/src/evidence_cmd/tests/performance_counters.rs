use super::*;

#[test]
fn performance_analysis_uses_existing_operational_evidence_without_implicit_payload_or_companion_reads() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("performance-counters", options);
        assert!(args.analysis == Analysis::PerformanceCounters);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
    let args = analyze_args(
        "performance-counters",
        &["--activity-range", "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"],
    );
    assert!(args.validate_selection().is_err());
}
