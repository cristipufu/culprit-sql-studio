use super::*;
use culprit_core::{
    Engine, ErrorDetail,
    telemetry::{
        CaptureIdentity, CollectionStop, DefinitionPayload, MAX_READ_TEMPLATES, PayloadUnavailable, REDACTION_VERSION,
        ReadQueryMetrics, ReadWorkMetrics, digest,
    },
};

fn metric(seed: &str, elapsed: f64) -> ReadQueryMetrics {
    ReadQueryMetrics {
        template_id: digest(seed.as_bytes()),
        statement: DefinitionPayload::sql("SELECT value FROM native_catalog WHERE id=9561", Engine::Postgres),
        completed_tasks: 4,
        failures: 1,
        timeouts: 1,
        cancellations: 0,
        unconfirmed_cancellations: 0,
        stop_causes: vec![],
        rows_decoded: "18446744073709551615".into(),
        charged_value_bytes: "9007199254740993".into(),
        client_elapsed_ms: elapsed,
    }
}
fn evidence() -> SourceOverheadEvidence {
    SourceOverheadEvidence {
        capture: CaptureIdentity { observation_id: "capture".into(), revision: None },
        engine: Some(Engine::Postgres),
        source: None,
        status: CollectionStatus::Complete,
        started_at: "2026-11-17T12:00:00.000000001Z".into(),
        finished_at: Some("2026-11-17T12:00:05.000000009Z".into()),
        source_queries: Some(91),
        read_metrics: Some(ReadWorkMetrics {
            native_session_work: None,
            active_tasks: 0,
            untracked_tasks: 40,
            templates: vec![metric("slower", 900.25), metric("faster", 20.0)],
        }),
    }
}

#[test]
fn exact_native_counts_and_original_total_survive_without_a_tracked_task_sum() {
    let input = evidence();
    let original = serde_json::to_value(&input.read_metrics).unwrap();
    let report = explain(&input).unwrap();
    assert_eq!(report.evidence.source_queries, Some(91));
    assert_eq!(report.template_order, [0, 1]);
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates[0].rows_decoded, "18446744073709551615");
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates[0].charged_value_bytes, "9007199254740993");
    assert_eq!(serde_json::to_value(&input.read_metrics).unwrap(), original);
    assert!(
        report.limitations.contains(&t!("view.source_overhead.limit.untracked", count = format::digits("40".into())))
    );
    assert!(report.limitations.contains(&t!("view.source_overhead.limit.native")));
    assert!(!serde_json::to_string(&report).unwrap().contains("9561"));
}

#[test]
fn no_metrics_is_distinct_from_a_recorded_empty_template_set() {
    let mut input = evidence();
    input.read_metrics = None;
    let missing = explain(&input).unwrap();
    assert_eq!(missing.summary, t!("view.source_overhead.summary.not_recorded"));
    assert!(missing.evidence.read_metrics.is_none());
    input.read_metrics =
        Some(ReadWorkMetrics { native_session_work: None, active_tasks: 0, untracked_tasks: 0, templates: vec![] });
    let empty = explain(&input).unwrap();
    assert!(empty.template_order.is_empty());
    assert_eq!(empty.evidence.source_queries, Some(91));
    assert_eq!(
        empty.summary,
        t!(
            "view.source_overhead.summary.recorded",
            templates = format::digits("0".into()),
            completed = format::digits("0".into())
        )
    );
}

#[test]
fn failed_and_unsettled_local_attempts_retain_their_status_and_incomplete_measurements() {
    let mut input = evidence();
    input.status = CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed);
    input.finished_at = None;
    input.source_queries = None;
    let metrics = input.read_metrics.as_mut().unwrap();
    metrics.active_tasks = 2;
    metrics.templates[0].unconfirmed_cancellations = 1;
    let report = explain(&input).unwrap();
    assert_eq!(report.evidence.status, input.status);
    assert!(report.evidence.finished_at.is_none());
    assert!(report.evidence.source_queries.is_none());
    assert!(report.limitations.contains(&t!("view.source_overhead.limit.incomplete")));
    assert!(report.limitations.contains(&t!("view.source_overhead.limit.active", count = format::digits("2".into()))));
    assert!(
        report.limitations.contains(&t!("view.source_overhead.limit.unconfirmed", count = format::digits("1".into())))
    );
    assert!(
        !report.limitations.contains(&t!("view.source_overhead.limit.archive")),
        "local attempts are not misdescribed as archive-only"
    );
}

#[test]
fn authenticated_but_unredacted_preview_is_withheld_without_mutating_the_original() {
    let mut input = evidence();
    let text = "SELECT 'credential-canary-761', 91236 /* parameter-canary */";
    input.read_metrics.as_mut().unwrap().templates[0].statement = DefinitionPayload::Available {
        text: text.into(),
        sha256: digest(text.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    assert!(input.read_metrics.as_ref().unwrap().is_valid());
    let report = explain(&input).unwrap();
    assert!(matches!(
        report.evidence.read_metrics.as_ref().unwrap().templates[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    ));
    let json = serde_json::to_string(&report).unwrap();
    for secret in ["credential-canary", "91236", "parameter-canary"] {
        assert!(!json.contains(secret));
    }
    assert!(serde_json::to_string(&input.read_metrics).unwrap().contains("credential-canary"));
    assert_eq!(
        report.evidence.read_metrics.as_ref().unwrap().templates[0].template_id,
        input.read_metrics.as_ref().unwrap().templates[0].template_id
    );
}

#[test]
fn unknown_engine_never_supplies_a_preview_redaction_dialect() {
    let mut input = evidence();
    input.engine = None;
    let report = explain(&input).unwrap();
    assert!(report.evidence.read_metrics.as_ref().unwrap().templates.iter().all(|metric| matches!(
        metric.statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    )));
    assert_eq!(report.evidence.source_queries, input.source_queries);
}

#[test]
fn unsafe_preview_withholding_is_idempotent_and_missing_previews_remain_native_unknowns() {
    let mut input = evidence();
    input.read_metrics.as_mut().unwrap().templates[0].statement =
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
    let first = explain(&input).unwrap();
    let second = explain(&input).unwrap();
    assert_eq!(serde_json::to_value(&first).unwrap(), serde_json::to_value(&second).unwrap());
    assert!(matches!(
        second.evidence.read_metrics.as_ref().unwrap().templates[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
}

#[test]
fn maximum_bounded_template_population_is_sorted_without_copying_metrics() {
    let mut input = evidence();
    input.read_metrics.as_mut().unwrap().templates =
        (0..MAX_READ_TEMPLATES).map(|index| metric(&index.to_string(), index as f64)).collect();
    let report = explain(&input).unwrap();
    assert_eq!(report.template_order.len(), MAX_READ_TEMPLATES);
    assert_eq!(report.template_order[0], (MAX_READ_TEMPLATES - 1) as u32);
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates.len(), MAX_READ_TEMPLATES);
    input.read_metrics.as_mut().unwrap().templates.push(metric("overflow", 1.0));
    assert_eq!(explain(&input).unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[test]
fn malformed_counters_clocks_duplicate_templates_and_nonfinite_time_are_rejected() {
    for scenario in ["counter", "clock", "unfinished_complete", "duplicate", "elapsed", "negative", "payload"] {
        let mut input = evidence();
        match scenario {
            "counter" => {
                input.read_metrics.as_mut().unwrap().templates[0].charged_value_bytes = "18446744073709551616".into()
            }
            "clock" => input.finished_at = Some("2026-11-16T00:00:00Z".into()),
            "unfinished_complete" => input.finished_at = None,
            "duplicate" => {
                let row = input.read_metrics.as_ref().unwrap().templates[0].clone();
                input.read_metrics.as_mut().unwrap().templates.push(row);
            }
            "elapsed" => input.read_metrics.as_mut().unwrap().templates[0].client_elapsed_ms = f64::INFINITY,
            "negative" => input.read_metrics.as_mut().unwrap().templates[0].client_elapsed_ms = -1.0,
            "payload" => {
                input.read_metrics.as_mut().unwrap().templates[0].statement = DefinitionPayload::Available {
                    text: "SELECT 1".into(),
                    sha256: digest(b"other"),
                    redaction_version: REDACTION_VERSION,
                }
            }
            _ => unreachable!(),
        }
        assert_eq!(
            explain(&input).unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{scenario}"
        );
    }
}

#[test]
fn valid_custom_source_credentials_and_paths_are_never_public_preview_authority() {
    for text in
        ["authorization=source_secret_canary", "password=bare_password_canary", "/private/credential/path_canary"]
    {
        let mut input = evidence();
        input.read_metrics.as_mut().unwrap().templates[0].statement = DefinitionPayload::Available {
            text: text.into(),
            sha256: digest(text.as_bytes()),
            redaction_version: REDACTION_VERSION,
        };
        assert!(input.read_metrics.as_ref().unwrap().is_valid());
        let report = explain(&input).unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains(text), "{text}");
        assert_eq!(
            report.evidence.read_metrics.as_ref().unwrap().templates[0].template_id,
            input.read_metrics.as_ref().unwrap().templates[0].template_id
        );
        assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates[0].client_elapsed_ms, 900.25);
    }
}

fn native_evidence() -> SourceOverheadEvidence {
    use culprit_core::{DatabaseIdentity, telemetry::*};
    let mut input = evidence();
    input.engine = None;
    let context = NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "private-reader-db", "private-reader-host.invalid", 1433, None)
            .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-reader-machine"),
                native: NativeIdentity::new("private-reader-instance"),
            }),
            epoch: Some("private-reader-process".into()),
        },
        observed_from: "2026-11-17T12:00:01.000000001Z".into(),
        observed_until: "2026-11-17T12:00:03.000000009Z".into(),
        database_writable: Some(true),
    };
    input.read_metrics.as_mut().unwrap().native_session_work =
        Some(Box::new(NativeSessionReadWork::MssqlSession(Box::new(MssqlSessionReadWork {
            settlement: NativeReadSettlement::Quiescent,
            issue: None,
            interval: Some(Box::new(MssqlSessionReadInterval {
                context,
                session: NativeActivityIdentity { id: "81".into(), generation: "2026-11-17T11:00:00.0030000".into() },
                reader_generation: "1".into(),
                initial: MssqlSessionReadPoint {
                    query_ordinal: 3,
                    cpu_ms: Some("10".into()),
                    reads: Some("9007199254740993".into()),
                    logical_reads: None,
                    dirty_marks: Some("1".into()),
                    memory_pages: Some("30".into()),
                },
                latest: Some(MssqlSessionReadPoint {
                    query_ordinal: 90,
                    cpu_ms: Some("10".into()),
                    reads: Some("9007199254740996".into()),
                    logical_reads: None,
                    dirty_marks: Some("2".into()),
                    memory_pages: Some("20".into()),
                }),
            })),
        }))));
    input
}

#[test]
fn recorded_profile_explains_exact_session_values_without_cost_attribution_or_private_authority() {
    let input = native_evidence();
    let original = serde_json::to_vec(input.read_metrics.as_ref().unwrap()).unwrap();
    let report = explain(&input).unwrap();
    let native = report.native_interval.as_ref().unwrap();
    assert_eq!(native.summary, t!("view.source_overhead.native.summary.settled"));
    assert!(native.measurements.contains(&t!(
        "view.source_overhead.native.counters",
        cpu = format::digits("0".into()),
        reads = format::digits("3".into()),
        logical = t!("view.source_overhead.native.unknown"),
        dirty = format::digits("1".into())
    )));
    assert!(native.measurements.contains(&t!(
        "view.source_overhead.native.memory",
        before = format::digits("30".into()),
        after = format::digits("20".into())
    )));
    for limitation in [
        t!("view.source_overhead.native.limit.interval"),
        t!("view.source_overhead.native.limit.io"),
        t!("view.source_overhead.native.limit.memory"),
        t!("view.source_overhead.native.limit.resolution"),
        t!("view.source_overhead.native.limit.mapping"),
    ] {
        assert!(native.limitations.contains(&limitation));
    }
    assert!(!report.limitations.contains(&t!("view.source_overhead.limit.native")));
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates[0].client_elapsed_ms, 900.25);
    assert_eq!(serde_json::to_vec(input.read_metrics.as_ref().unwrap()).unwrap(), original);
    let public = serde_json::to_string(&report).unwrap();
    for canary in ["private-reader", "2026-11-17T11:00:00.0030000", "reader_generation", "native-session-id"] {
        assert!(!public.contains(canary), "{canary}");
    }
}

#[test]
fn native_provisional_shutdown_and_poison_states_withhold_all_numeric_deltas() {
    use culprit_core::telemetry::*;
    for (settlement, issue) in [
        (NativeReadSettlement::Pending, None),
        (NativeReadSettlement::Unconfirmed, None),
        (NativeReadSettlement::Quiescent, Some(NativeReadWorkIssue::CounterDecreased)),
        (NativeReadSettlement::Unconfirmed, Some(NativeReadWorkIssue::SourceChanged)),
    ] {
        let mut input = native_evidence();
        let NativeSessionReadWork::MssqlSession(body) =
            input.read_metrics.as_mut().unwrap().native_session_work.as_deref_mut().unwrap();
        body.settlement = settlement;
        body.issue = issue;
        let report = explain(&input).unwrap();
        let native = report.native_interval.as_ref().unwrap();
        assert_eq!(
            native.summary,
            if settlement == NativeReadSettlement::Pending && issue.is_none() {
                t!("view.source_overhead.native.summary.observing")
            } else {
                t!("view.source_overhead.native.summary.unavailable")
            }
        );
        let NativeSessionReadSummary::MssqlSession { interval: Some(interval), .. } =
            report.evidence.read_metrics.as_ref().unwrap().native_session_work.as_deref().unwrap()
        else {
            panic!()
        };
        assert!(
            interval.cpu_ms.is_none()
                && interval.reads.is_none()
                && interval.memory_pages_initial.is_none()
                && interval.memory_pages_latest.is_none()
        );
        assert_eq!(native.measurements.len(), 2, "only original proof and observed bounds remain");
        if issue == Some(NativeReadWorkIssue::CounterDecreased) {
            assert!(native.limitations.contains(&t!("view.source_overhead.native.issue.decreased")));
        }
        if settlement == NativeReadSettlement::Unconfirmed {
            assert!(native.limitations.contains(&t!("view.source_overhead.native.limit.unconfirmed")));
        }
    }
}
