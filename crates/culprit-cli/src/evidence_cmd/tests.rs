use super::*;
use clap::Parser;

#[test]
fn archive_commands_need_only_query_configuration_exact_capture_and_explicit_selection() {
    let range = culprit_core::telemetry::digest(b"capture");
    let base = ["culprit", "collect", "evidence", "read", "--config", "archive.json", "--range", &range];
    assert!(crate::Cli::try_parse_from(base).is_err());
    let parsed =
        crate::Cli::try_parse_from(base.into_iter().chain(["--family", "operational-samples", "--json"])).unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Read(args))) =
        parsed.command
    else {
        panic!()
    };
    assert_eq!(
        args.common.request(args.families.into_iter().map(Into::into).collect(), Format::Json).unwrap().families,
        [EvidenceFamily::OperationalSamples]
    );
    let parsed = crate::Cli::try_parse_from([
        "culprit",
        "collect",
        "evidence",
        "analyze",
        "--config",
        "archive.json",
        "--range",
        &range,
        "--analysis",
        "queries",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Analyze(args))) =
        parsed.command
    else {
        panic!()
    };
    assert_eq!(args.analysis.families().len(), 4);
    for name in [
        "memory-failures",
        "native-failures",
        "request-memory",
        "session-memory",
        "page-waits",
        "task-space",
        "statement-work",
        "runtime-distributions",
    ] {
        let parsed = crate::Cli::try_parse_from([
            "culprit",
            "collect",
            "evidence",
            "analyze",
            "--config",
            "archive.json",
            "--range",
            &range,
            "--analysis",
            name,
        ])
        .unwrap();
        let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Analyze(args))) =
            parsed.command
        else {
            panic!()
        };
        assert_eq!(args.analysis.families(), [EvidenceFamily::OperationalSamples]);
    }
    assert!(
        crate::Cli::try_parse_from(base.into_iter().chain([
            "--family",
            "operational-samples",
            "--connection",
            "production"
        ]))
        .is_err()
    );
}

#[test]
fn invalid_selection_budgets_ranges_and_output_limits_fail_before_opening_configuration() {
    let parsed = crate::Cli::try_parse_from([
        "culprit",
        "collect",
        "evidence",
        "read",
        "--config",
        "missing.json",
        "--range",
        "../capture",
        "--family",
        "operational-samples",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Read(mut args))) =
        parsed.command
    else {
        panic!()
    };
    assert!(args.common.request(vec![EvidenceFamily::OperationalSamples], Format::Text).is_err());
    args.common.range = culprit_core::telemetry::digest(b"capture");
    assert!(args.common.request(vec![EvidenceFamily::OperationalSamples], Format::Md).is_err());
    assert!(args.common.request(vec![EvidenceFamily::OperationalSamples; 2], Format::Json).is_err());
    args.common.budget.page_rows = 0;
    assert!(args.common.request(vec![EvidenceFamily::OperationalSamples], Format::Json).is_err());
    args.common.budget.page_rows = 1;
    args.common.max_output_bytes = crate::bounded_output::MAX_JSON_BYTES + 1;
    assert!(args.common.request(vec![EvidenceFamily::OperationalSamples], Format::Json).is_err());
}

#[test]
fn human_summaries_bound_the_number_and_length_of_finding_titles() {
    let mut text = read_summary("range", 2, 1);
    crate::output::append_brief_titles(&mut text, ["title"; 8].into_iter());
    assert_eq!(text.matches('•').count(), crate::output::BRIEF_FINDING_LIMIT);
    let mut text = String::new();
    let long = "x".repeat(1000);
    crate::output::append_brief_titles(&mut text, [long.as_str()].into_iter());
    assert_eq!(text.chars().filter(|character| *character == 'x').count(), crate::output::BRIEF_TITLE_CHARS);
}
