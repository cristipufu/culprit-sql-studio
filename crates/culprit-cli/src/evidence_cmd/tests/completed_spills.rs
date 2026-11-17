use super::*;

#[test]
fn completed_spills_selects_optional_definitions_without_a_second_read_budget() {
    let range = culprit_core::telemetry::digest(b"original-definitions");
    for recovery in [false, true] {
        let mut options = vec!["--definitions-range", range.as_str(), "--max-queries", "7"];
        if recovery {
            options.extend(["--with-recovery-payloads", "--max-recovery-objects", "2"]);
        }
        let args = analyze_args("completed-spills", &options);
        args.validate_selection().unwrap();
        assert_eq!(args.definitions_range.as_deref(), Some(range.as_str()));
        let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert_eq!(request.budget.max_queries, 7);
        assert_eq!(request.budget.recovery_payloads.map(|budget| budget.max_objects), recovery.then_some(2));
        assert!(args.activity_range.is_none() && args.blocking_range.is_none() && args.compilation_baseline.is_none());
    }
    assert!(analyze_args("completed-spills", &[]).validate_selection().is_ok());
}

#[tokio::test]
async fn invalid_same_or_unrelated_definition_ranges_fail_before_config_or_credentials() {
    let original = culprit_core::telemetry::digest(b"archive-population-capture");
    for range in ["../outside", "", original.as_str()] {
        let mut args = analyze_args("completed-spills", &["--definitions-range", range]);
        args.common.config = std::env::temp_dir().join("culprit-spill-config-must-not-open.json");
        assert_eq!(
            run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract))
        );
    }
    let definition = culprit_core::telemetry::digest(b"definition");
    for mode in ["queries", "cached-plans", "statement-work", "source-overhead", "maintenance-progress"] {
        let args = analyze_args(mode, &["--definitions-range", &definition]);
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.message, t!("evidence.cli.definitions_only"));
    }
}
