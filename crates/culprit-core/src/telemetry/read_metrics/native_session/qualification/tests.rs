use super::*;
use crate::{Engine, telemetry::ReadMetricsRecorder};
use std::time::Duration;

fn metrics(queries: u32) -> ReadWorkMetrics {
    let recorder = ReadMetricsRecorder::new(Engine::Mssql);
    for _ in 0..queries {
        let id = recorder.begin("SELECT /* culprit */ N'private-paid-phase-canary'");
        recorder.finish(id.as_deref(), Duration::ZERO, &ReadProgress::default(), &ReadTaskOutcome::default());
    }
    recorder.snapshot().unwrap()
}
fn native() -> NativeSessionReadWork {
    super::super::tests::work()
}
fn proof() -> (ReadWorkMetrics, MssqlQualificationReadWork) {
    let mut source = metrics(9);
    let NativeSessionReadWork::MssqlSession(mut original) = native();
    original.interval.as_mut().unwrap().initial.query_ordinal = 0;
    source.native_session_work = Some(Box::new(NativeSessionReadWork::MssqlSession(original.clone())));
    let mut preparation = metrics(2);
    let mut prepared = original.clone();
    prepared.settlement = NativeReadSettlement::Pending;
    let interval = prepared.interval.as_mut().unwrap();
    interval.initial.query_ordinal = 2;
    interval.latest = None;
    interval.context.observed_from = "2020-01-01T11:59:59Z".into();
    interval.context.observed_until = interval.context.observed_from.clone();
    preparation.native_session_work = Some(Box::new(NativeSessionReadWork::MssqlSession(prepared)));
    let mut observer = metrics(6);
    let interval = original.interval.as_mut().unwrap();
    interval.initial.query_ordinal = 2;
    interval.latest.as_mut().unwrap().query_ordinal = 6;
    interval.session.id = "72".into();
    observer.native_session_work = Some(Box::new(NativeSessionReadWork::MssqlSession(original)));
    (
        source,
        MssqlQualificationReadWork {
            aggregate_budget: CollectionBudget::default(),
            preparation: ReadPhaseAccounting {
                queries: 2,
                metrics: Box::new(preparation),
                session_setup: Box::new(metrics(1)),
            },
            observer: ReadPhaseAccounting {
                queries: 6,
                metrics: Box::new(observer),
                session_setup: Box::new(metrics(1)),
            },
        },
    )
}
#[test]
fn paid_phases_preserve_real_setup_and_original_native_identity_without_measuring_setup_cpu() {
    let (source, paid) = proof();
    assert!(paid.is_valid());
    assert!(paid.covers_source(&source, 9));
    let recovered: MssqlQualificationReadWork = serde_json::from_slice(&serde_json::to_vec(&paid).unwrap()).unwrap();
    assert_eq!(recovered, paid);
    assert_eq!(paid.preparation.queries, 2);
    assert_eq!(tasks(&paid.preparation.session_setup), 1);
    assert!(paid.preparation.session_setup.native_session_work.is_none());
    assert_eq!(
        paid.preparation.metrics.native_session_work.as_deref().unwrap().settlement(),
        NativeReadSettlement::Pending
    );
}
#[test]
fn aggregate_includes_source_preparation_observer_and_both_actual_set_batches() {
    let (source, mut paid) = proof();
    paid.aggregate_budget.max_queries = 19;
    assert!(paid.covers_source(&source, 9));
    paid.aggregate_budget.max_queries = 18;
    assert!(!paid.covers_source(&source, 9));
    paid.aggregate_budget.max_queries = 19;
    paid.observer.metrics.templates[0].charged_value_bytes = paid.aggregate_budget.total_bytes.to_string();
    paid.preparation.metrics.templates[0].charged_value_bytes = "1".into();
    assert!(!paid.covers_source(&source, 9));
}
#[test]
fn missing_foreign_reused_or_unsettled_phase_proof_never_covers_the_original_window() {
    let (source, paid) = proof();
    let mut invalid_source = source.clone();
    invalid_source.templates[0].client_elapsed_ms = f64::INFINITY;
    assert!(!paid.covers_source(&invalid_source, 9));
    for case in [
        "missing",
        "login",
        "generation",
        "role",
        "member",
        "clock",
        "active",
        "failure",
        "untracked",
        "count",
        "setup-error",
    ] {
        let mut changed = paid.clone();
        match case {
            "missing" => changed.preparation.metrics.native_session_work = None,
            "active" => changed.observer.metrics.active_tasks = 1,
            "failure" => changed.observer.metrics.templates[0].failures = 1,
            "untracked" => changed.observer.metrics.untracked_tasks = 1,
            "count" => changed.preparation.queries += 1,
            "setup-error" => {
                let metric = &mut changed.preparation.session_setup.templates[0];
                metric.failures = 1;
                metric.unconfirmed_cancellations = 1;
            }
            _ => {
                let NativeSessionReadWork::MssqlSession(work) =
                    changed.preparation.metrics.native_session_work.as_deref_mut().unwrap();
                let interval = work.interval.as_mut().unwrap();
                match case {
                    "login" => interval.session.generation = "2020-01-01T09:00:00.0000000".into(),
                    "generation" => interval.reader_generation = "2".into(),
                    "role" => interval.context.database_writable = Some(false),
                    "clock" => interval.context.observed_until = "2020-01-01T12:00:02Z".into(),
                    _ => {
                        let SourcePartition::InstanceLocal { member: Some(member), .. } =
                            &mut interval.context.partition
                        else {
                            panic!()
                        };
                        member.native.id.push_str("foreign");
                    }
                }
            }
        }
        assert!(!changed.covers_source(&source, 9), "{case}");
    }
}
#[test]
fn nested_qualification_is_rejected_before_recursive_validation() {
    let (_, mut paid) = proof();
    let nested = paid.clone();
    let NativeSessionReadWork::MssqlSession(work) =
        paid.preparation.metrics.native_session_work.as_deref_mut().unwrap();
    work.qualification_work = Some(Box::new(nested));
    assert!(!paid.is_valid());
}
#[test]
fn public_summary_keeps_only_opaque_receipt_and_existing_numerical_allowlist() {
    let (mut source, paid) = proof();
    let NativeSessionReadWork::MssqlSession(work) = source.native_session_work.as_deref_mut().unwrap();
    work.qualification_work = Some(Box::new(paid));
    assert!(source.is_valid());
    let public = serde_json::to_value(source.summary(None).unwrap()).unwrap();
    let text = public.to_string();
    assert!(!text.contains("private-paid-phase-canary"));
    assert!(!text.contains("private-native-cost-db-canary"));
    assert!(!text.contains("qualification_work"));
    assert!(!text.contains("session_setup"));
    let NativeSessionReadSummary::MssqlSession { reference, .. } =
        source.summary(None).unwrap().native_session_work.map(|work| *work).unwrap();
    assert_eq!(reference, digest(&serde_json::to_vec(source.native_session_work.as_deref().unwrap()).unwrap()));
}
#[test]
fn malformed_and_oversized_auxiliary_metrics_do_not_expand_native_evidence_limits() {
    let (mut source, mut paid) = proof();
    paid.observer.metrics.templates[0].client_elapsed_ms = f64::INFINITY;
    assert!(!paid.is_valid());
    let (_, mut paid) = proof();
    let text = "private-large-native-evidence ".repeat(280);
    paid.observer.metrics.templates[0].statement = DefinitionPayload::Available {
        sha256: digest(text.as_bytes()),
        text: text.clone(),
        redaction_version: crate::telemetry::REDACTION_VERSION,
    };
    paid.preparation.metrics.templates[0].statement = DefinitionPayload::Available {
        sha256: digest(text.as_bytes()),
        text,
        redaction_version: crate::telemetry::REDACTION_VERSION,
    };
    let NativeSessionReadWork::MssqlSession(work) = source.native_session_work.as_deref_mut().unwrap();
    work.qualification_work = Some(Box::new(paid));
    assert!(!source.is_valid());
}
