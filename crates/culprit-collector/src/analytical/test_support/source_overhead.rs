//! Synthetic source-task accounting for metadata replay; no SQL is sent or native cost qualified.
use culprit_core::{
    Engine,
    telemetry::{CollectionStop, ReadMetricsRecorder, ReadProgress, ReadTaskOutcome, ReadWorkMetrics, digest},
};
use std::{sync::atomic::Ordering, time::Duration};

pub const SOURCE_OVERHEAD_QUERY_COUNT: u32 = 100;

pub fn source_overhead_read_metrics() -> ReadWorkMetrics {
    let recorder = ReadMetricsRecorder::new(Engine::Postgres);
    let progress = ReadProgress::default();
    progress.rows.store(u64::MAX, Ordering::Relaxed);
    progress.bytes.store(9_007_199_254_740_993, Ordering::Relaxed);
    for index in 0..SOURCE_OVERHEAD_QUERY_COUNT {
        let id = recorder
            .begin(&format!("/* culprit */ SELECT native_value FROM overhead_preview_canary WHERE native_id={index}"));
        let outcome =
            if index == 3 { ReadTaskOutcome::stopped(CollectionStop::TimedOut) } else { ReadTaskOutcome::default() };
        recorder.finish(id.as_deref(), Duration::from_micros(u64::from(index + 1) * 1_000 + 125), &progress, &outcome);
    }
    let metrics = recorder.snapshot().expect("synthetic recorder lock is not poisoned");
    assert!(metrics.is_valid());
    metrics
}

/// Independent synthetic source-session authority. A shared dataset does not identify its connected database.
pub fn native_source_overhead_read_metrics() -> ReadWorkMetrics {
    use culprit_core::{DatabaseIdentity, telemetry::*};
    let mut metrics = source_overhead_read_metrics();
    let context = NativeReadContext {
        target: DatabaseIdentity::server(
            Engine::Mssql,
            "native-cost-reader-db-canary",
            "native-cost-reader-host-canary.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-cost-reader-machine-canary"),
                native: NativeIdentity::new("native-cost-reader-instance-canary"),
            }),
            epoch: Some("native-cost-reader-process-canary".into()),
        },
        observed_from: "2020-01-01T12:00:00.000000001Z".into(),
        observed_until: "2020-01-01T12:00:01.000000009Z".into(),
        database_writable: Some(true),
    };
    metrics.native_session_work = Some(Box::new(NativeSessionReadWork::MssqlSession(Box::new(MssqlSessionReadWork {
        qualification_work: None,
        settlement: NativeReadSettlement::Quiescent,
        issue: None,
        interval: Some(Box::new(MssqlSessionReadInterval {
            context,
            session: NativeActivityIdentity { id: "81".into(), generation: "2020-01-01T11:30:00.0030000".into() },
            reader_generation: "1".into(),
            initial: MssqlSessionReadPoint {
                query_ordinal: 3,
                cpu_ms: Some("10".into()),
                reads: Some("9007199254740993".into()),
                logical_reads: Some("100".into()),
                dirty_marks: Some("2".into()),
                memory_pages: Some("30".into()),
            },
            latest: Some(MssqlSessionReadPoint {
                query_ordinal: 90,
                cpu_ms: Some("17".into()),
                reads: Some("9007199254740996".into()),
                logical_reads: Some("110".into()),
                dirty_marks: Some("3".into()),
                memory_pages: Some("20".into()),
            }),
        })),
    }))));
    assert!(metrics.is_valid());
    metrics
}

pub fn assert_native_source_overhead_summary(public: &serde_json::Value, private: &ReadWorkMetrics) {
    let text = serde_json::to_string(public).unwrap();
    for canary in [
        "native-cost-reader-db-canary",
        "native-cost-reader-host-canary.invalid",
        "native-cost-reader-machine-canary",
        "native-cost-reader-instance-canary",
        "native-cost-reader-process-canary",
        "2020-01-01T11:30:00.0030000",
        "overhead_preview_canary",
    ] {
        assert!(!text.contains(canary), "{canary}");
    }
    let native = private.native_session_work.as_ref().unwrap();
    assert!(text.contains(&digest(&serde_json::to_vec(native).unwrap())));
    assert_eq!(public["native_session_work"]["interval"]["cpu_ms"], "7");
    assert_eq!(public["native_session_work"]["interval"]["reads"], "3");
    assert_eq!(public["native_session_work"]["interval"]["memory_pages_initial"], "30");
    assert_eq!(public["native_session_work"]["interval"]["memory_pages_latest"], "20");
    let stopped = public["templates"].as_array().unwrap().iter().find(|metric| metric["timeouts"] == 1).unwrap();
    assert_eq!(stopped["stop_causes"], serde_json::json!([{"reason":"timed-out","tasks":1}]));
}

pub fn connection_source_overhead_read_metrics() -> ReadWorkMetrics {
    use culprit_core::telemetry::*;
    let mut metrics = source_overhead_read_metrics();
    let phase = |stage, outcome, template_id| ReadConnectionPhase {
        stage,
        outcome,
        template_id,
        client_started_at: "2020-01-01T12:00:00Z".into(),
        client_finished_at: Some("2020-01-01T12:00:01Z".into()),
        client_elapsed_ms: Some(1000.0),
    };
    metrics.connection_work = Some(Box::new(ReadConnectionWork {
        attempted: 2,
        omitted_attempts: 0,
        omitted_phases: 0,
        active_phases: 0,
        unconfirmed_phases: 0,
        attempts: vec![
            ReadConnectionAttempt {
                ordinal: 1,
                connection_use: ReadConnectionUse::FreshConnection,
                reader_generation: None,
                omitted_phases: 0,
                phases: vec![
                    phase(
                        ReadConnectionStage::Acquire,
                        ReadConnectionOutcome::Failed { code: culprit_core::ErrorCode::AuthFailed, native_code: None },
                        None,
                    ),
                    phase(
                        ReadConnectionStage::HandshakeAndLogin,
                        ReadConnectionOutcome::Failed {
                            code: culprit_core::ErrorCode::AuthFailed,
                            native_code: Some(18456),
                        },
                        None,
                    ),
                ],
            },
            ReadConnectionAttempt {
                ordinal: 2,
                connection_use: ReadConnectionUse::AuthenticatedReuse,
                reader_generation: Some("1".into()),
                omitted_phases: 0,
                phases: vec![
                    phase(ReadConnectionStage::Acquire, ReadConnectionOutcome::Completed, None),
                    phase(
                        ReadConnectionStage::FirstQuery,
                        ReadConnectionOutcome::Failed {
                            code: culprit_core::ErrorCode::PermissionDenied,
                            native_code: None,
                        },
                        Some(digest(b"original-query-template")),
                    ),
                ],
            },
        ],
    }));
    assert!(metrics.is_valid());
    metrics
}
