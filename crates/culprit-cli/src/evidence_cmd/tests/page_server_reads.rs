use super::super::selection::EvidenceSelection;
use super::*;

#[test]
fn page_server_reads_selects_only_runtime_under_the_original_single_action_budget() {
    let command = EvidenceCommand::Analyze(analyze_args(
        "page-server-reads",
        &["--max-queries", "7", "--max-rows", "11", "--timeout-ms", "1234"],
    ));
    let selection = EvidenceSelection::from_command(command).unwrap();
    assert!(selection.plan.analysis == Some(Analysis::PageServerReads));
    assert_eq!(selection.plan.families, [EvidenceFamily::QueryRuntime]);
    let request = selection.common.request(selection.plan.families, Format::Json).unwrap();
    assert_eq!(request.budget.max_queries, 7);
    assert_eq!(request.budget.max_rows, 11);
    assert_eq!(request.budget.timeout_ms, 1234);
    assert!(request.budget.recovery_payloads.is_none());
    let companions = selection.plan.companions;
    assert!(
        companions.activity.is_none()
            && companions.blocking.is_none()
            && companions.compilation_baseline.is_none()
            && companions.definitions.is_none()
    );
}

#[tokio::test]
async fn page_server_reads_refuses_unrelated_companions_before_configuration_or_credentials() {
    let range = culprit_core::telemetry::digest(b"unrelated original companion");
    for (flag, key) in [
        ("--activity-range", "evidence.cli.activity_page_waits_only"),
        ("--compilation-baseline", "evidence.cli.compilation_baseline_only"),
        ("--blocking-range", "evidence.cli.blocking_range_only"),
        ("--workload-population", "evidence.cli.population_queries_only"),
    ] {
        let mut args = analyze_args("page-server-reads", &[flag, &range]);
        args.common.config = std::env::temp_dir().join("culprit-page-server-configuration-must-not-open.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput, "{flag}");
        assert_eq!(error.message, culprit_core::i18n::t(key, &[]), "{flag}");
    }
}

#[test]
fn an_explicit_page_server_definition_companion_keeps_the_same_single_action_and_independent_family() {
    let range = culprit_core::telemetry::digest(b"independent-query-store-definitions");
    let args = analyze_args(
        "page-server-reads",
        &[
            "--definitions-range",
            &range,
            "--max-queries",
            "7",
            "--max-rows",
            "11",
            "--timeout-ms",
            "1234",
            "--page-rows",
            "1",
        ],
    );
    let selection = EvidenceSelection::from_command(EvidenceCommand::Analyze(args)).unwrap();
    let request = selection.common.request(selection.plan.families.clone(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::QueryRuntime]);
    assert_eq!(request.budget.max_queries, 7);
    assert_eq!(request.budget.max_rows, 11);
    assert_eq!(request.budget.timeout_ms, 1234);
    assert!(request.budget.recovery_payloads.is_none());
    let companion = selection.plan.companions.definitions.unwrap();
    assert_eq!(companion.range, range);
    assert_eq!(companion.kind, super::super::selection::DefinitionKind::QueryStoreRuntime);
    assert_eq!(companion.kind.family(), EvidenceFamily::QueryDefinitions);
    let case = culprit_collector::analytical::test_support::page_server_read_evidence_cases().remove(0);
    assert!(companion.kind.accepts_primary(&case.source));
    let compiler = culprit_collector::analytical::test_support::compilation_definition_evidence_cases().remove(0);
    assert!(!companion.kind.accepts_primary(&compiler.current.0));
}

#[test]
fn only_an_explicit_page_server_companion_may_select_distinct_families_from_the_same_capture_range() {
    let original = culprit_core::telemetry::digest(b"archive-population-capture");
    let args = analyze_args(
        "page-server-reads",
        &["--definitions-range", &original, "--max-queries", "7", "--max-rows", "11"],
    );
    let selection = EvidenceSelection::from_command(EvidenceCommand::Analyze(args)).unwrap();
    let request = selection.common.request(selection.plan.families.clone(), Format::Json).unwrap();
    assert_eq!(selection.common.range, original);
    let companion = selection.plan.companions.definitions.unwrap();
    assert_eq!(companion.range, original);
    assert_eq!(request.families, [EvidenceFamily::QueryRuntime]);
    assert_eq!(companion.kind.family(), EvidenceFamily::QueryDefinitions);
    assert_eq!(request.budget.max_queries, 7);
    assert_eq!(request.budget.max_rows, 11);
    assert!(request.budget.recovery_payloads.is_none());
    for analysis in ["compilation", "completed-spills"] {
        assert!(analyze_args(analysis, &["--definitions-range", &original]).validate_selection().is_err());
    }
}
