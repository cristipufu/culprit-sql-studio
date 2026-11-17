use super::*;

#[test]
fn maintenance_blocking_selects_explicit_distinct_ranges_without_expanding_recovery_allowance() {
    let later = culprit_core::telemetry::digest(b"later-native-blocking");
    for (options, format) in [
        (vec!["--blocking-range", later.as_str()], Format::Text),
        (vec!["--blocking-range", later.as_str(), "--json"], Format::Json),
        (
            vec![
                "--blocking-range",
                later.as_str(),
                "--with-recovery-payloads",
                "--max-recovery-objects",
                "2",
                "--max-queries",
                "7",
            ],
            Format::Json,
        ),
    ] {
        let args = analyze_args("maintenance-blocking", &options);
        args.validate_selection().unwrap();
        assert_eq!(args.blocking_range.as_deref(), Some(later.as_str()));
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        if options.contains(&"--with-recovery-payloads") {
            assert_eq!(request.budget.max_queries, 7);
            assert_eq!(request.budget.recovery_payloads.unwrap().max_objects, 2);
        } else {
            assert!(request.budget.recovery_payloads.is_none());
        }
        assert!(args.activity_range.is_none() && args.compilation_baseline.is_none());
    }
}

#[tokio::test]
async fn missing_invalid_or_same_blocking_range_fails_before_configuration_and_credentials() {
    let current = culprit_core::telemetry::digest(b"archive-population-capture");
    for range in [None, Some("../capture"), Some(""), Some(current.as_str())] {
        let mut options = Vec::new();
        if let Some(range) = range {
            options.extend(["--blocking-range", range]);
        }
        let mut args = analyze_args("maintenance-blocking", &options);
        args.common.config = std::env::temp_dir().join("culprit-maintenance-blocking-config-must-not-be-opened.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        if range.is_none() {
            assert_eq!(error.message, t!("evidence.cli.maintenance_blocking_required"));
        } else {
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
        }
    }
}

#[tokio::test]
async fn a_blocking_companion_cannot_be_borrowed_by_other_analysis_modes() {
    let later = culprit_core::telemetry::digest(b"later-native-blocking");
    for mode in ["queries", "blocking", "maintenance", "maintenance-progress", "page-waits", "compilation"] {
        let mut args = analyze_args(mode, &["--blocking-range", &later]);
        args.common.config = std::env::temp_dir().join("culprit-unrelated-blocking-config-must-not-be-opened.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.message, t!("evidence.cli.blocking_range_only"), "{mode}");
    }
}

#[tokio::test]
async fn maintenance_blocking_cannot_substitute_a_workload_population_activity_parent_or_compilation_baseline() {
    let later = culprit_core::telemetry::digest(b"later-native-blocking");
    let unrelated = culprit_core::telemetry::digest(b"unrelated-original-range");
    for (extra, key) in [
        (vec!["--activity-range", unrelated.as_str()], "evidence.cli.activity_page_waits_only"),
        (vec!["--compilation-baseline", unrelated.as_str()], "evidence.cli.compilation_baseline_only"),
        (vec!["--workload-population", "17"], "evidence.cli.population_queries_only"),
    ] {
        let mut options = vec!["--blocking-range", later.as_str()];
        options.extend(extra);
        let args = analyze_args("maintenance-blocking", &options);
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.message, culprit_core::i18n::t(key, &[]));
    }
}
