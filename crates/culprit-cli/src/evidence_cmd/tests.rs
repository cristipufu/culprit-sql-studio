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
        "compilation",
        "cleanup",
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

#[test]
fn tuning_uses_the_existing_archive_route_and_selects_only_its_typed_stream() {
    let range = culprit_core::telemetry::digest(b"tuning-capture");
    for (mode, selection, value) in [("read", "--family", "tuning-observations"), ("analyze", "--analysis", "tuning")] {
        let parsed = crate::Cli::try_parse_from([
            "culprit",
            "collect",
            "evidence",
            mode,
            "--config",
            "archive.json",
            "--range",
            &range,
            selection,
            value,
            "--json",
        ])
        .unwrap();
        let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(command)) = parsed.command else {
            panic!("archive command")
        };
        let (common, families) = match command {
            EvidenceCommand::Read(args) => (args.common, args.families.into_iter().map(Into::into).collect()),
            EvidenceCommand::Analyze(args) => (args.common, args.analysis.families()),
        };
        let request = common.request(families, Format::Json).unwrap();
        assert_eq!(request.families, [EvidenceFamily::TuningObservations]);
        assert!(request.budget.recovery_payloads.is_none());
    }
}

#[test]
fn tuning_text_uses_existing_source_qualification_and_bounded_titles_without_payload_bodies() {
    use culprit_core::{QueryStoreScope, SourceOutcome, SourceState, TuningExplanation, TuningReport, i18n::Message};
    let entry = TuningExplanation {
        source: "native-source".into(),
        observation_key: "forced:3:9:unknown".into(),
        scope: Some(QueryStoreScope::Unknown),
        title: Message::from("x".repeat(1000)),
        explanation: Message::from("native-context-only"),
        investigation: Message::from("verify exact scope"),
        verify: Message::from("capture again"),
    };
    let report = TuningReport {
        sources: vec![SourceOutcome {
            source: "native-source".into(),
            state: SourceState::Partial,
            reason: None,
            reason_message: None,
        }],
        title: Message::from("Recorded tuning"),
        summary: Message::from("Retained native observations"),
        limitations: vec![Message::from("Unknown scope remains unknown")],
        entries: vec![entry; 20],
    };
    let output = observations::tuning(&report);
    assert_eq!(output.matches('•').count(), crate::output::BRIEF_FINDING_LIMIT);
    assert_eq!(
        output.chars().filter(|character| *character == 'x').count(),
        crate::output::BRIEF_FINDING_LIMIT * crate::output::BRIEF_TITLE_CHARS
    );
    assert!(output.contains("native-source") && output.contains("Unknown scope remains unknown"));
    assert!(!output.contains("native-context-only"));
}

fn analyze_args(analysis: &str, options: &[&str]) -> AnalyzeArgs {
    let range = culprit_core::telemetry::digest(b"archive-population-capture");
    let parsed = crate::Cli::try_parse_from(
        [
            "culprit",
            "collect",
            "evidence",
            "analyze",
            "--config",
            "missing.json",
            "--range",
            &range,
            "--analysis",
            analysis,
        ]
        .into_iter()
        .chain(options.iter().copied()),
    )
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Analyze(args))) =
        parsed.command
    else {
        panic!("archive query analysis")
    };
    args
}

fn population_catalog(identities: Vec<NativeIdentity>) -> NativeRuntimePopulationCatalog {
    NativeRuntimePopulationCatalog {
        source: "native-query-history".into(),
        partitioned: true,
        requires_selection: identities.len() > 1,
        populations: identities
            .into_iter()
            .map(|identity| culprit_core::NativeRuntimePopulation { identity, native_properties: Default::default() })
            .collect(),
    }
}

#[test]
fn query_analysis_defaults_to_all_independent_sections_and_accepts_an_exact_native_id() {
    let args = analyze_args("queries", &[]);
    assert!(args.workload_population.is_none());
    assert_eq!(resolve_population(None, args.workload_population.as_ref()).unwrap(), None);
    let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
    assert_eq!(request.families.len(), 4);

    let args = analyze_args("queries", &["--workload-population", "native-reader-17"]);
    assert_eq!(args.workload_population, Some(NativeIdentity::new("native-reader-17")));
    let identity = NativeIdentity::new("native-reader-17").qualified("native-generation", "original-context");
    let catalog = population_catalog(vec![identity.clone(), NativeIdentity::new("native-reader-18")]);
    assert_eq!(resolve_population(Some(&catalog), args.workload_population.as_ref()).unwrap(), Some(identity));
}

#[test]
fn invalid_archive_group_ids_are_rejected_by_argument_parsing_without_configuration_access() {
    let range = culprit_core::telemetry::digest(b"archive-population-capture");
    let base = [
        "culprit",
        "collect",
        "evidence",
        "analyze",
        "--config",
        "missing.json",
        "--range",
        &range,
        "--analysis",
        "queries",
    ];
    for id in ["".to_owned(), "bad\nidentity".to_owned(), "x".repeat(513)] {
        assert!(crate::Cli::try_parse_from(base.into_iter().chain(["--workload-population", id.as_str()])).is_err());
    }
    assert!(crate::Cli::try_parse_from(base.into_iter().chain(["--workload-population"])).is_err());
    assert!(
        crate::Cli::try_parse_from(base.into_iter().chain([
            "--workload-population",
            "17",
            "--workload-population",
            "18"
        ]))
        .is_err()
    );
    assert_eq!(
        analyze_args("queries", &["--workload-population", &"x".repeat(512)]).workload_population.unwrap().id.len(),
        512
    );
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
            "query-runtime",
            "--workload-population",
            "17",
        ])
        .is_err()
    );
}

#[tokio::test]
async fn group_selection_on_non_query_analysis_stops_before_opening_configuration() {
    for mode in [
        "memory-failures",
        "native-failures",
        "request-memory",
        "session-memory",
        "page-waits",
        "task-space",
        "statement-work",
        "runtime-distributions",
        "cached-plans",
        "compilation",
        "cleanup",
        "tuning",
    ] {
        let mut args = analyze_args(mode, &["--workload-population", "17"]);
        args.common.config =
            std::env::temp_dir().join(format!("culprit-invalid-archive-population-{}.json", std::process::id()));
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
        assert_eq!(error.message, t!("evidence.cli.population_queries_only"));
    }
    let mut args = analyze_args("queries", &[]);
    args.workload_population = Some(NativeIdentity::new(""));
    let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
    assert_eq!(error.message, t!("cli.scan.workload_population_invalid"));
}

#[test]
fn archive_group_resolution_rejects_missing_unknown_and_ambiguous_identities_without_guessing_properties() {
    let requested = NativeIdentity::new("17");
    let mut catalog = population_catalog(vec![NativeIdentity::new("18")]);
    catalog.populations[0].native_properties.insert("replica_group_id".into(), "17".into());
    for catalog in [None, Some(&catalog)] {
        let error = resolve_population(catalog, Some(&requested)).unwrap_err();
        assert_eq!(error.message, t!("evidence.cli.population_missing", id = "17"));
    }
    let catalog = NativeRuntimePopulationCatalog {
        source: "native-query-history".into(),
        partitioned: false,
        requires_selection: false,
        populations: vec![],
    };
    assert!(resolve_population(Some(&catalog), Some(&requested)).is_err());
    let catalog = population_catalog(vec![
        requested.clone().qualified("context", "first"),
        requested.clone().qualified("context", "second"),
    ]);
    assert!(catalog.is_valid());
    let error = resolve_population(Some(&catalog), Some(&requested)).unwrap_err();
    assert_eq!(error.message, t!("evidence.cli.population_ambiguous", id = "17"));
    assert_eq!(
        resolve_population(Some(&catalog), None).unwrap(),
        None,
        "the default delegates every independent group to the pure API"
    );
}
