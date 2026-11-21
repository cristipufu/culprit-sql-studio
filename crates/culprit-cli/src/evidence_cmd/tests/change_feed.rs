use super::*;
#[test]
fn change_feed_archive_selection_uses_only_original_rows_and_rejects_implicit_companions() {
    let args = analyze_args("change-feed", &["--json"]);
    assert!(args.analysis == Analysis::ChangeFeed);
    args.validate_selection().unwrap();
    let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
    assert!(request.budget.recovery_payloads.is_none());
    for option in
        ["--activity-range", "--definitions-range", "--compilation-baseline", "--cache-baseline", "--blocking-range"]
    {
        let args = analyze_args("change-feed", &[option, &"a".repeat(64)]);
        assert!(args.validate_selection().is_err(), "{option}");
    }
}
