use super::*;
use clap::Parser;
mod compilation_definitions;
mod completed_spills;
mod configured_log_rates;
mod connection_outcomes;
mod database_properties;
mod maintenance_blocking;
mod memory_clerks;
mod page_server_reads;
mod page_task_holders;
mod performance_counters;
mod source_overhead;
mod storage;

#[test]
fn cli_exposes_every_verified_evidence_family() {
    let actual = Family::value_variants().iter().copied().map(EvidenceFamily::from).collect::<Vec<_>>();
    assert_eq!(actual.len(), EvidenceFamily::ALL.len());
    for family in EvidenceFamily::ALL {
        assert_eq!(actual.iter().filter(|candidate| *candidate == family).count(), 1);
    }
}

#[test]
fn replication_analysis_and_explicit_reads_select_only_the_replication_stream() {
    for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("replication", options);
        assert!(args.analysis == Analysis::Replication);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::ReplicationSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
    let range = culprit_core::telemetry::digest(b"replication-capture");
    let parsed = crate::Cli::try_parse_from([
        "culprit",
        "collect",
        "evidence",
        "read",
        "--config",
        "archive.json",
        "--range",
        &range,
        "--family",
        "replication-samples",
        "--json",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Read(args))) =
        parsed.command
    else {
        panic!("replication archive read")
    };
    let families = args.families.into_iter().map(Into::into).collect();
    assert_eq!(args.common.request(families, Format::Json).unwrap().families, [EvidenceFamily::ReplicationSamples]);
}

#[test]
fn blocking_analysis_selects_only_original_activity_rows_and_keeps_payload_reads_explicit() {
    for (options, format) in [
        (&["--json"][..], Format::Json),
        (&[][..], Format::Text),
        (&["--json", "--with-recovery-payloads"][..], Format::Json),
    ] {
        let args = analyze_args("blocking", options);
        assert!(args.analysis == Analysis::Blocking);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::ActivityObservations]);
        assert_eq!(request.budget.recovery_payloads.is_some(), options.contains(&"--with-recovery-payloads"));
    }
}

#[tokio::test]
async fn blocking_rejects_unrelated_population_and_activity_range_selectors_before_configuration_or_credentials() {
    for (options, key) in [
        (vec!["--workload-population", "17"], "evidence.cli.population_queries_only"),
        (
            vec!["--activity-range", "019abc019abc019abc019abc019abc019abc019abc019abc019abc019abc019abc0"],
            "evidence.cli.activity_page_waits_only",
        ),
    ] {
        let mut args = analyze_args("blocking", &options);
        args.common.config = std::env::temp_dir().join("culprit-blocking-configuration-must-not-be-read.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
        assert_eq!(error.message, culprit_core::i18n::t(key, &[]));
    }
    let mut args = analyze_args("blocking", &[]);
    args.common.range = "../capture".into();
    let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
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
    for (name, family) in
        [("cached-plans", EvidenceFamily::ObjectDefinitions), ("maintenance", EvidenceFamily::ActivityObservations)]
    {
        let args = analyze_args(name, &["--json"]);
        assert_eq!(args.analysis.families(), [family]);
    }
    for name in [
        "memory-failures",
        "native-failures",
        "request-memory",
        "session-memory",
        "page-waits",
        "page-wait-writers",
        "page-task-holders",
        "task-space",
        "statement-work",
        "statement-grants",
        "runtime-distributions",
        "compilation",
        "compiler-admission",
        "maintenance-progress",
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
fn maintenance_text_retains_native_phase_and_exact_counters_without_private_read_context() {
    use culprit_core::{MaintenanceProgressEntry, MaintenanceProgressReport, Metric, SourceState, telemetry::*};
    let exact = format::digits("9007199254740993".into());
    let phase = "building index: scanning table";
    let report = MaintenanceProgressReport {
        source: "pg_stat_progress_create_index".into(),
        context: NativeReadContext {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Postgres,
                "db",
                "private-maintenance-cli-route.invalid",
                5432,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        },
        source_state: SourceState::Partial,
        source_reason: None,
        read_complete: true,
        native_rows: 1,
        observed_rows: 1,
        binding: FocusedRequestBinding::UnverifiedOriginalScope,
        membership: FocusedRequestMembership::Observed,
        title: t!("view.maintenance_progress.title"),
        summary: t!("view.maintenance_progress.summary.unverified"),
        limitations: vec![t!("view.maintenance_progress.limit.point")],
        entries: vec![MaintenanceProgressEntry {
            observation_key: digest(b"native-maintenance-point"),
            actor: ActivityKey {
                session: NativeActivityIdentity { id: "61".into(), generation: "native-login".into() },
                request: Some(NativeActivityIdentity { id: "statement".into(), generation: "native-start".into() }),
                transaction: None,
            },
            database_oid: "16384".into(),
            relation: None,
            index: None,
            command: MaintenanceCommand::CreateIndex,
            phase: phase.into(),
            wait: None,
            title: t!("view.maintenance_progress.entry.title", session = "61"),
            explanation: t!("view.maintenance_progress.entry.postgres"),
            metrics: vec![Metric::said_text(
                culprit_core::key!("view.maintenance_progress.metric.blocks_total"),
                exact.clone(),
            )],
            investigation: t!("view.maintenance_progress.investigate"),
            verify: t!("view.maintenance_progress.verify"),
        }],
    };
    let text = observations::maintenance_progress(&report);
    assert!(text.contains(phase) && text.contains(&exact));
    assert!(text.contains(&t!("evidence.cli.source_partial")));
    assert!(!text.contains("private-maintenance-cli-route"));
    assert!(!text.contains("9007199254740992"));
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

#[test]
fn cached_statement_grants_use_operational_archive_reads_with_explicit_recovery_opt_in() {
    for options in [vec![], vec!["--with-recovery-payloads"]] {
        let args = analyze_args("statement-grants", &options);
        assert!(args.analysis == Analysis::StatementGrants);
        let request = args.common.request(args.analysis.families(), Format::Json).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert_eq!(request.budget.recovery_payloads.is_some(), !options.is_empty());
    }
}

#[test]
fn cached_grant_text_preserves_independent_qualification_and_bounds_entry_explanations() {
    use culprit_core::{
        StatementGrantExplanation, StatementGrantReport, i18n::Message, telemetry::CachedStatementIdentity,
    };
    let entry = StatementGrantExplanation {
        snapshot_key: "read-key".into(),
        observation_key: "entry-key".into(),
        identity: CachedStatementIdentity {
            plan_handle: "0x0102".into(),
            sql_handle: "0x0304".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2020-01-01T01:00:00.003".into(),
            plan_generation: "1".into(),
        },
        title: Message::from("X".repeat(1500)),
        explanation: Message::from("Y".repeat(2000)),
        metrics: vec![],
        investigation: Message::from("capture simultaneous participants"),
        verify: Message::from("compare representative work"),
    };
    let report = StatementGrantReport {
        sources: vec![culprit_core::SourceOutcome {
            source: "native-grants".into(),
            state: culprit_core::SourceState::Partial,
            reason: None,
            reason_message: None,
        }],
        reads: vec![],
        title: Message::from("Cached grant history"),
        summary: Message::from("Retained native entries"),
        limitations: vec![Message::from("Native lifetime counters do not identify current pressure owners")],
        entries: vec![entry; 20],
    };
    let output = observations::statement_grants(&report);
    assert_eq!(output.matches('•').count(), crate::output::BRIEF_FINDING_LIMIT);
    assert!(output.chars().filter(|character| *character == 'X').count() < 1500 * crate::output::BRIEF_FINDING_LIMIT);
    assert!(!output.contains(&"Y".repeat(2000)));
    assert!(
        output.contains("native-grants")
            && output.contains("Native lifetime counters do not identify current pressure owners")
    );
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

#[test]
fn compiler_baseline_selects_one_distinct_authenticated_range_and_rejects_other_analysis_kinds() {
    let baseline = culprit_core::telemetry::digest(b"earlier-native-compilation-read");
    let args = analyze_args("compilation", &["--compilation-baseline", &baseline]);
    assert!(args.validate_selection().is_ok());
    assert_eq!(args.compilation_baseline.as_deref(), Some(baseline.as_str()));
    assert!(analyze_args("queries", &["--compilation-baseline", &baseline]).validate_selection().is_err());
    assert!(analyze_args("compilation", &["--compilation-baseline", "not-a-capture"]).validate_selection().is_err());
    let current = culprit_core::telemetry::digest(b"archive-population-capture");
    assert!(analyze_args("compilation", &["--compilation-baseline", &current]).validate_selection().is_err());
}

#[test]
fn original_activity_selection_is_an_independent_exact_archive_range() {
    let parent = culprit_core::telemetry::digest(b"original-activity-capture");
    let args = analyze_args("page-waits", &["--activity-range", &parent]);
    args.validate_selection().unwrap();
    assert_eq!(args.activity_range.as_deref(), Some(parent.as_str()));
    assert_eq!(args.analysis.families(), [EvidenceFamily::OperationalSamples]);
    assert!(args.common.budget.read_budget().recovery_payloads.is_none());
}

#[tokio::test]
async fn unrelated_or_invalid_activity_ranges_fail_before_configuration_and_credentials() {
    let parent = culprit_core::telemetry::digest(b"original-activity-capture");
    for analysis in ["queries", "maintenance", "request-memory", "page-wait-writers"] {
        let args = analyze_args(analysis, &["--activity-range", &parent]);
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.message, t!("evidence.cli.activity_page_waits_only"));
    }
    let same = culprit_core::telemetry::digest(b"archive-population-capture");
    for range in ["../range", "", same.as_str()] {
        let args = analyze_args("page-waits", &["--activity-range", range]);
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    }
}

fn population_catalog(identities: Vec<NativeIdentity>) -> NativeRuntimePopulationCatalog {
    NativeRuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
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
        "compiler-admission",
        "maintenance-progress",
        "maintenance",
        "cleanup",
        "tuning",
        "replication",
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
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
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

#[test]
fn native_wait_counter_read_selects_its_canonical_family_without_automatic_recovery_or_analysis() {
    let range = culprit_core::telemetry::digest(b"native-wait-counter-capture");
    let parsed = crate::Cli::try_parse_from([
        "culprit",
        "collect",
        "evidence",
        "read",
        "--config",
        "archive.json",
        "--range",
        &range,
        "--family",
        "wait-counter-samples",
        "--json",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Evidence(EvidenceCommand::Read(args))) =
        parsed.command
    else {
        panic!("native wait counter archive read")
    };
    let request = args.common.request(args.families.into_iter().map(Into::into).collect(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::WaitCounterSamples]);
    assert!(request.budget.recovery_payloads.is_none());
}

#[test]
fn native_counter_ratio_analysis_uses_only_operational_evidence_with_existing_cumulative_budget() {
    for (flags, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("counter-ratios", flags);
        assert!(args.analysis == Analysis::CounterRatios);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
}

#[test]
fn wait_counter_analysis_selects_only_the_existing_wait_family_without_payload_hydration() {
    for (flags, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
        let args = analyze_args("wait-counters", flags);
        assert!(args.analysis == Analysis::WaitCounters);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::WaitCounterSamples]);
        assert!(request.budget.recovery_payloads.is_none());
    }
}
