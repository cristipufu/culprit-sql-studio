use super::*;
use clap::Parser;

#[test]
fn local_overhead_accepts_only_an_explicit_outbox_and_observation_without_source_or_archive_configuration() {
    let cli = crate::Cli::try_parse_from([
        "culprit",
        "collect",
        "outbox",
        "overhead",
        "local-outbox",
        "aabbccddeeff00112233445566778899",
        "--json",
    ])
    .unwrap();
    let crate::Command::Collect(crate::collect_cmd::CollectCommand::Outbox(OutboxCommand::Overhead {
        directory,
        observation,
    })) = cli.command
    else {
        panic!("local overhead command")
    };
    assert_eq!(directory, PathBuf::from("local-outbox"));
    assert_eq!(observation, "aabbccddeeff00112233445566778899");
    for forbidden in ["--config", "--connection", "--with-recovery-payloads", "--source-quiescent"] {
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "outbox",
                "overhead",
                "local-outbox",
                "capture",
                forbidden,
                "private-config"
            ])
            .is_err()
        );
    }
    assert!(crate::Cli::try_parse_from(["culprit", "collect", "outbox", "overhead", "local-outbox"]).is_err());
}

#[test]
fn local_overhead_uses_the_shared_safe_json_and_human_formatter_without_exporting_original_native_sql() {
    use culprit_core::{SourceOverheadEvidence, telemetry::*};
    let private = "Authorization: Bearer private-collector-credential /private/native-route original_activity_query";
    let evidence = SourceOverheadEvidence {
        capture: CaptureIdentity { observation_id: "a".repeat(32), revision: None },
        engine: None,
        source: None,
        status: CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed),
        started_at: "2020-01-01T00:00:00Z".into(),
        finished_at: Some("2020-01-01T00:00:01Z".into()),
        source_queries: Some(1),
        read_metrics: Some(ReadWorkMetrics {
            connection_work: None,
            native_session_work: None,
            active_tasks: 1,
            untracked_tasks: 1,
            templates: vec![ReadQueryMetrics {
                template_id: digest(b"exact source template"),
                statement: DefinitionPayload::Available {
                    text: private.into(),
                    sha256: digest(private.as_bytes()),
                    redaction_version: REDACTION_VERSION,
                },
                completed_tasks: 1,
                failures: 1,
                timeouts: 0,
                cancellations: 1,
                unconfirmed_cancellations: 1,
                stop_causes: vec![],
                rows_decoded: "9007199254740993".into(),
                charged_value_bytes: "18446744073709551615".into(),
                client_elapsed_ms: 125.0,
            }],
        }),
    };
    let report = culprit_checks::source_overhead::explain(&evidence).unwrap();
    let json = encode(&report).unwrap().to_string();
    let text = crate::output::observations::source_overhead(&report);
    for output in [&json, &text] {
        assert!(output.contains("9007199254740993"));
        assert!(output.contains("18446744073709551615"));
        assert!(output.contains(&digest(b"exact source template")));
        for canary in [private, "private-collector-credential", "native-route", "original_activity_query"] {
            assert!(!output.contains(canary));
        }
    }
    assert!(json.contains("cancellation-unconfirmed"));
    assert_eq!(report.evidence.engine, None);
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().active_tasks, 1);
}
