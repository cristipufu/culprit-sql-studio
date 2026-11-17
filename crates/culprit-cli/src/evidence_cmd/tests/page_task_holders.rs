use super::*;

#[test]
fn holder_analysis_reads_only_operational_facts_with_one_explicit_recovery_allowance() {
    for (options, format) in [
        (vec![], Format::Text),
        (vec!["--json"], Format::Json),
        (vec!["--with-recovery-payloads", "--max-recovery-objects", "2", "--max-queries", "7"], Format::Json),
    ] {
        let args = analyze_args("page-task-holders", &options);
        assert!(args.analysis == Analysis::PageTaskHolders);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        if options.contains(&"--with-recovery-payloads") {
            assert_eq!(request.budget.max_queries, 7);
            assert_eq!(request.budget.recovery_payloads.unwrap().max_objects, 2);
        } else {
            assert!(request.budget.recovery_payloads.is_none());
        }
        assert!(args.activity_range.is_none() && args.compilation_baseline.is_none() && args.blocking_range.is_none());
    }
}

#[tokio::test]
async fn holders_cannot_borrow_unrelated_parents_or_populations_before_configuration_is_opened() {
    let range = culprit_core::telemetry::digest(b"unrelated-capture");
    for (extra, key) in [
        (vec!["--activity-range", range.as_str()], "evidence.cli.activity_page_waits_only"),
        (vec!["--compilation-baseline", range.as_str()], "evidence.cli.compilation_baseline_only"),
        (vec!["--blocking-range", range.as_str()], "evidence.cli.blocking_range_only"),
        (vec!["--workload-population", "17"], "evidence.cli.population_queries_only"),
    ] {
        let mut args = analyze_args("page-task-holders", &extra);
        args.common.config = std::env::temp_dir().join("culprit-holder-config-must-not-be-opened.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.message, culprit_core::i18n::t(key, &[]));
    }
}

#[tokio::test]
async fn invalid_archive_locator_or_budget_is_rejected_without_opening_a_source_or_archive() {
    for invalid_range in [true, false] {
        let mut args = analyze_args("page-task-holders", &[]);
        args.common.config = std::env::temp_dir().join("culprit-holder-config-must-not-be-opened.json");
        if invalid_range {
            args.common.range = "../source-database".into();
        } else {
            args.common.budget.max_queries = 0;
        }
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        let problem = if invalid_range { DeliveryProblem::InvalidContract } else { DeliveryProblem::Capacity };
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(problem)));
    }
}
