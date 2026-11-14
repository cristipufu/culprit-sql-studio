use super::*;
use clap::Parser;

#[test]
fn cli_exposes_every_verified_evidence_family() {
    let actual = Family::value_variants().iter().copied().map(EvidenceFamily::from).collect::<Vec<_>>();
    assert_eq!(actual.len(), EvidenceFamily::ALL.len());
    for family in EvidenceFamily::ALL {
        assert_eq!(actual.iter().filter(|candidate| *candidate == family).count(), 1);
    }
}

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
        "cached-plans",
        "--json",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Analyze(args))) =
        parsed.command
    else {
        panic!()
    };
    assert_eq!(args.analysis.families(), [EvidenceFamily::ObjectDefinitions]);
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

fn recovery_read_args(options: &[&str]) -> ReadArgs {
    let range = culprit_core::telemetry::digest(b"capture");
    let parsed = crate::Cli::try_parse_from(
        [
            "culprit",
            "collect",
            "evidence",
            "read",
            "--config",
            "missing.json",
            "--range",
            &range,
            "--family",
            "object-definitions",
        ]
        .into_iter()
        .chain(options.iter().copied()),
    )
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Read(args))) =
        parsed.command
    else {
        panic!("archive read command")
    };
    args
}

#[test]
fn raw_recovery_reads_require_explicit_opt_in_and_keep_separate_object_and_byte_limits() {
    let args = recovery_read_args(&[]);
    assert!(
        args.common
            .request(vec![EvidenceFamily::ObjectDefinitions], Format::Json)
            .unwrap()
            .budget
            .recovery_payloads
            .is_none()
    );
    let args = recovery_read_args(&["--with-recovery-payloads"]);
    let limits = args
        .common
        .request(vec![EvidenceFamily::ObjectDefinitions], Format::Json)
        .unwrap()
        .budget
        .recovery_payloads
        .unwrap();
    assert_eq!(limits.max_objects, RecoveryPayloadBudget::default().max_objects);
    assert_eq!(limits.max_bytes, RecoveryPayloadBudget::default().max_bytes);
    let args = recovery_read_args(&[
        "--with-recovery-payloads",
        "--max-recovery-objects",
        "2",
        "--max-recovery-bytes",
        "16384",
    ]);
    let limits = args
        .common
        .request(vec![EvidenceFamily::ObjectDefinitions], Format::Json)
        .unwrap()
        .budget
        .recovery_payloads
        .unwrap();
    assert_eq!((limits.max_objects, limits.max_bytes), (2, 16384));
    for option in ["--max-recovery-objects", "--max-recovery-bytes"] {
        let range = culprit_core::telemetry::digest(b"capture");
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "evidence",
                "read",
                "--config",
                "missing.json",
                "--range",
                &range,
                "--family",
                "object-definitions",
                option,
                "1",
            ])
            .is_err()
        );
    }
}

#[tokio::test]
async fn invalid_recovery_limits_fail_before_the_missing_configuration_is_opened() {
    for options in [
        ["--with-recovery-payloads", "--max-recovery-objects", "0"],
        ["--with-recovery-payloads", "--max-recovery-objects", "129"],
        ["--with-recovery-payloads", "--max-recovery-bytes", "0"],
        ["--with-recovery-payloads", "--max-recovery-bytes", "268435457"],
    ] {
        let mut args = recovery_read_args(&options);
        args.common.config =
            std::env::temp_dir().join(format!("culprit-invalid-recovery-budget-{}.json", std::process::id()));
        let error = run(EvidenceCommand::Read(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(culprit_core::telemetry::DeliveryProblem::Capacity))
        );
    }
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
