use super::*;

#[test]
fn configured_limits_read_only_operational_rows_with_no_payload_or_companion_reads() {
    let args = analyze_args("configured-log-rates", &["--json"]);
    assert!(args.analysis == Analysis::ConfiguredLogRates);
    args.validate_selection().unwrap();
    let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
    assert!(request.budget.recovery_payloads.is_none());
    let args = analyze_args(
        "configured-log-rates",
        &["--activity-range", "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"],
    );
    assert!(args.validate_selection().is_err());
}
