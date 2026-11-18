use super::*;

#[test]
fn session_census_selects_only_original_activity_bodies_and_keeps_the_common_read_budget() {
    for format in [Format::Json, Format::Text] {
        let args = analyze_args("session-census", &["--max-rows", "7", "--max-queries", "2"]);
        assert!(args.analysis == Analysis::SessionCensus);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::ActivityObservations]);
        assert_eq!(request.budget.max_rows, 7);
        assert_eq!(request.budget.max_queries, 2);
    }
}

#[tokio::test]
async fn unrelated_population_options_are_rejected_before_archive_configuration_credentials_or_database_access() {
    let mut args = analyze_args("session-census", &["--workload-population", "17"]);
    args.common.config = std::env::temp_dir().join("culprit-census-must-not-read-config.json");
    let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
    assert_eq!(error.message, culprit_core::i18n::t("evidence.cli.population_queries_only", &[]));
}
