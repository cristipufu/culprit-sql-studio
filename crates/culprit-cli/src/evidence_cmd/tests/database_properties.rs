use super::*;
#[test]
fn properties_analysis_reads_only_original_operational_evidence_without_companions_or_payload_hydration() {
    let args = analyze_args("database-properties", &["--json"]);
    assert!(args.analysis == Analysis::DatabaseProperties);
    args.validate_selection().unwrap();
    let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
    assert!(request.budget.recovery_payloads.is_none());
    assert!(analyze_args("database-properties", &["--workload-population", "1"]).validate_selection().is_err());
}
