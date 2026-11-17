use super::*;

#[test]
fn source_overhead_selects_metadata_only_and_preserves_explicit_archive_read_budgets() {
    for options in [vec![], vec!["--json"], vec!["--with-recovery-payloads", "--json"]] {
        let args = analyze_args("source-overhead", &options);
        assert!(args.analysis == Analysis::SourceOverhead);
        args.validate_selection().unwrap();
        assert!(args.analysis.families().is_empty());
        let budget = args.common.validate(Format::Json).unwrap();
        assert_eq!(budget.recovery_payloads.is_some(), options.contains(&"--with-recovery-payloads"));
        assert!(
            args.common.request(args.analysis.families(), Format::Json).is_err(),
            "metadata never becomes an empty native-family request"
        );
    }
}

#[tokio::test]
async fn invalid_overhead_selectors_and_bounds_fail_before_configuration_or_credentials() {
    let range = culprit_core::telemetry::digest(b"other-capture");
    for options in [
        vec!["--workload-population", "17"],
        vec!["--activity-range", &range],
        vec!["--compilation-baseline", &range],
        vec!["--blocking-range", &range],
    ] {
        let mut args = analyze_args("source-overhead", &options);
        args.common.config = std::env::temp_dir().join("culprit-overhead-config-must-not-be-read.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
    }
    for invalid_range in [true, false] {
        let mut args = analyze_args("source-overhead", &[]);
        args.common.config = std::env::temp_dir().join("culprit-overhead-config-must-not-be-read.json");
        if invalid_range {
            args.common.range = "../capture".into();
        } else {
            args.common.budget.max_queries = 0;
        }
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(if invalid_range {
                DeliveryProblem::InvalidContract
            } else {
                DeliveryProblem::Capacity
            }))
        );
    }
}
