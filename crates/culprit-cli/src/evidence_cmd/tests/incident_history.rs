use super::*;
#[test]
fn incident_history_is_an_actual_closed_archive_analysis_over_only_operational_revisions() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("incident-history", options);
        assert!(args.analysis == Analysis::IncidentHistory);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
}
#[tokio::test]
async fn current_population_or_source_companions_cannot_be_attached_to_incident_history() {
    let mut args = analyze_args("incident-history", &["--workload-population", "17"]);
    args.common.config = std::env::temp_dir().join("history-credentials-must-not-be-opened.json");
    let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
    assert_eq!(error.message, t!("evidence.cli.population_queries_only"));
}
