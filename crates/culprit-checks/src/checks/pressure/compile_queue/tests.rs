use super::*;
use crate::views::compilation::tests::{native as retained_native, observation};
use crate::waits::snapshot_with;
use culprit_core::{
    CompilationMemory, Pressure, QueryCosts, Signal, SignalUnit, SourceOutcome, signals::CompileHistory, telemetry::*,
};

fn observed_queue() -> Snapshot {
    let mut snapshot = snapshot_with(Engine::Mssql);
    snapshot.taken_at = "2026-11-13T10:00:00Z".into();
    let mut signal = Signal::new(signal_id::COMPILATION_WAITING, "fixture", 2.0, SignalUnit::Count, SignalScope::Now);
    signal.measurement_scope = Some(MeasurementScope::Database);
    snapshot.pressure = Some(Pressure { signals: vec![signal], ..Default::default() });
    let mut database = snapshot.database_identity();
    database.incarnation = Some("native-compilation-database".into());
    snapshot.identity = Some(database.clone());
    let mut row = observation("5");
    row.source = culprit_core::source_id::QUERY_COMPILATION.into();
    let native = retained_native(&mut row);
    native.last_compile_at = "2026-11-13T09:59:00Z".into();
    native.memory =
        Some(CompilationMemory { last_kb: Some(32768.0), maximum_kb: Some(1048576.0), ..Default::default() });
    let identity = native.identity.clone();
    snapshot.operations.query_compilation = Some(CompilationSnapshot {
        source: row.source.clone(),
        context: Some(NativeReadContext {
            target: database.into(),
            partition: SourcePartition::Target,
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:00.000000001Z".into(),
            database_writable: Some(true),
        }),
        // Capture wrapper clocks remain independent of the original native context.
        observed_from: "2026-11-13T10:01:00Z".into(),
        observed_until: "2026-11-13T10:01:01Z".into(),
        observations: vec![row],
    });
    snapshot.sources.push(SourceOutcome {
        source: culprit_core::source_id::QUERY_COMPILATION.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    let query = application_query(&mut snapshot);
    query.id = identity.query_id.clone();
    query.native_identity = Some(identity);
    query.costs = Some(QueryCosts {
        compile_history: Some(CompileHistory {
            count: 5.0,
            last_at: Some("2026-11-13T09:59:00Z".into()),
            memory: Some(CompilationMemory {
                last_kb: Some(32768.0),
                maximum_kb: Some(1048576.0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    snapshot
}

fn application_query(snapshot: &mut Snapshot) -> &mut QueryStat {
    snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|query| !is_monitoring_query(query)).unwrap()
}

fn captured(snapshot: &mut Snapshot) -> &mut CompilationSnapshot {
    snapshot.operations.query_compilation.as_mut().unwrap()
}

fn compilation(snapshot: &mut Snapshot) -> &mut MssqlQueryCompilation {
    retained_native(&mut captured(snapshot).observations[0])
}

fn queue_without_candidates(snapshot: &Snapshot) {
    let findings = CompileQueueCheck.run(snapshot);
    assert_eq!(findings.len(), 1, "missing candidate evidence cannot hide the independently observed queue");
    assert!(findings[0].queries.is_empty());
    assert_eq!(findings[0].title, "2 requests are waiting to compile");
    assert!(!findings[0].fix.as_ref().unwrap().summary.is_empty());
}

#[test]
fn a_database_compiler_queue_links_recent_memory_heavy_compiles_as_candidates_without_asserting_ownership() {
    let snapshot = observed_queue();
    let findings = CompileQueueCheck.run(&snapshot);
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.title, "2 requests are waiting to compile");
    assert_eq!(finding.severity, Severity::Low);
    assert_eq!(finding.queries.len(), 1);
    let candidate = finding.queries[0].note.as_deref().unwrap();
    assert_eq!(
        candidate,
        "Investigation candidate: its last compilation used 32.0 MB at 2026-11-13T09:59:00Z; current ownership is unknown."
    );
    assert_eq!(
        finding.summary,
        "Compiler admission is delaying these requests; a gateway’s concurrency slots or memory budget can be exhausted while server RAM remains available."
    );
    assert_eq!(finding.fix.as_ref().unwrap().summary, t!("check.compile_queue.fix"));
    assert_eq!(finding.verify, [t!("check.compile_queue.verify")]);
    assert!(!candidate.contains("1.1 GB"), "the historical maximum is not the latest compilation");
    assert!(finding.caveats[0].contains("memory holders"));
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    assert!(finding.summary.contains("slots"));
}

#[test]
fn shared_unknown_execution_or_empty_queues_do_not_establish_database_compiler_pressure() {
    for scope in [None, Some(MeasurementScope::Server), Some(MeasurementScope::Pool)] {
        let mut snapshot = observed_queue();
        snapshot.pressure.as_mut().unwrap().signals[0].measurement_scope = scope;
        assert!(CompileQueueCheck.run(&snapshot).is_empty());
    }
    let mut snapshot = observed_queue();
    snapshot.pressure.as_mut().unwrap().signals[0].key = signal_id::MEMORY_GRANTS_PENDING.into();
    assert!(CompileQueueCheck.run(&snapshot).is_empty(), "execution grants are a different allocator");
    let mut snapshot = observed_queue();
    snapshot.pressure.as_mut().unwrap().signals[0].value = 0.0;
    assert!(CompileQueueCheck.run(&snapshot).is_empty());
}

#[test]
fn old_future_unmeasured_or_peak_only_memory_never_identifies_a_recent_candidate() {
    for (last_at, last_kb) in [
        ("2026-11-13T09:00:00Z", Some(32768.0)),
        ("2026-11-13T10:01:00Z", Some(32768.0)),
        ("2026-11-13T09:59:00Z", None),
        ("2026-11-13T09:59:00Z", Some(f64::NAN)),
    ] {
        let mut snapshot = observed_queue();
        let native = compilation(&mut snapshot);
        native.last_compile_at = last_at.into();
        native.memory.as_mut().unwrap().last_kb = last_kb;
        let finding = CompileQueueCheck.run(&snapshot).remove(0);
        assert!(finding.queries.is_empty());
        assert!(!finding.fix.unwrap().summary.is_empty(), "missing attribution still needs a next step");
    }
}

#[test]
fn original_native_clock_and_memory_survive_client_clock_skew_and_projected_history_disagreement() {
    for client_time in ["2000-01-01T00:00:00Z", "2040-01-01T00:00:00Z"] {
        let mut snapshot = observed_queue();
        snapshot.taken_at = client_time.into();
        let read = captured(&mut snapshot);
        read.observed_from = client_time.into();
        read.observed_until = client_time.into();
        // Projected costs can be absent; they do not supply the native lifetime, namespace or memory.
        application_query(&mut snapshot).costs = None;
        let finding = CompileQueueCheck.run(&snapshot).remove(0);
        assert_eq!(finding.queries.len(), 1, "{client_time}");
        assert!(finding.queries[0].note.as_ref().unwrap().contains("32.0 MB"));
        let query = application_query(&mut snapshot);
        query.costs = Some(QueryCosts {
            compile_history: Some(CompileHistory {
                count: 1000.0,
                last_at: Some("2041-01-01T00:00:00Z".into()),
                memory: Some(CompilationMemory { last_kb: Some(1048576.0), ..Default::default() }),
                ..Default::default()
            }),
            ..Default::default()
        });
        let finding = CompileQueueCheck.run(&snapshot).remove(0);
        assert!(finding.queries[0].note.as_ref().unwrap().contains("32.0 MB"));
        assert!(!finding.queries[0].note.as_ref().unwrap().contains("1.1 GB"));
    }
}

#[test]
fn projected_history_missing_native_clocks_and_unqualified_namespaces_cannot_lend_a_candidate() {
    let mut projected_only = observed_queue();
    projected_only.operations.query_compilation = None;
    queue_without_candidates(&projected_only);
    for missing in
        ["context", "source", "native-incarnation", "workload-incarnation", "foreign-namespace", "query-identity"]
    {
        let mut snapshot = observed_queue();
        match missing {
            "context" => captured(&mut snapshot).context = None,
            "source" => snapshot.sources.retain(|source| source.source != culprit_core::source_id::QUERY_COMPILATION),
            "native-incarnation" => {
                let CollectionTarget::Database(database) =
                    &mut captured(&mut snapshot).context.as_mut().unwrap().target
                else {
                    panic!()
                };
                database.incarnation = None;
            }
            "workload-incarnation" => snapshot.identity.as_mut().unwrap().incarnation = None,
            "foreign-namespace" => {
                let CollectionTarget::Database(database) =
                    &mut captured(&mut snapshot).context.as_mut().unwrap().target
                else {
                    panic!()
                };
                database.incarnation = Some("another-native-database".into());
            }
            _ => application_query(&mut snapshot).native_identity = None,
        }
        queue_without_candidates(&snapshot);
    }
}

#[test]
fn exact_recent_interval_rejects_future_compilations_and_native_clock_regression_without_float_rounding() {
    for (last_at, expected) in [
        ("2026-11-13T09:55:00.000000001Z", 1),
        ("2026-11-13T09:55:00Z", 0),
        ("2026-11-13T12:00:00.000000001+02:00", 1),
        ("2026-11-13T10:00:00.000000002Z", 0),
    ] {
        let mut snapshot = observed_queue();
        compilation(&mut snapshot).last_compile_at = last_at.into();
        let finding = CompileQueueCheck.run(&snapshot).remove(0);
        assert_eq!(finding.queries.len(), expected, "{last_at}");
    }
    for changed in ["regressing-clock", "malformed-clock", "reported-skew", "compile-lifetime"] {
        let mut snapshot = observed_queue();
        match changed {
            "regressing-clock" => {
                captured(&mut snapshot).context.as_mut().unwrap().observed_from = "2026-11-13T10:00:01Z".into()
            }
            "malformed-clock" => {
                captured(&mut snapshot).context.as_mut().unwrap().observed_until = "2026-11-13T10:00:00".into()
            }
            "reported-skew" => compilation(&mut snapshot).seconds_since_first_compile = -1,
            _ => {
                let raw = &mut captured(&mut snapshot).observations[0];
                let native = retained_native(raw);
                native.identity.qualifiers.insert("initial_compile_at".into(), "2026-11-13T09:59:30Z".into());
                let identity = native.identity.clone();
                raw.key = native.key().unwrap();
                application_query(&mut snapshot).native_identity = Some(identity);
            }
        }
        queue_without_candidates(&snapshot);
    }
}

#[test]
fn query_id_text_or_hash_cannot_substitute_for_the_complete_unique_native_query_incarnation() {
    for (qualifier, changed) in [
        ("context_settings_id", "6"),
        ("object_id", "2000"),
        ("query_hash", "FEDCBA9876543210"),
        ("initial_compile_at", "2026-11-13T09:58:30Z"),
    ] {
        let mut snapshot = observed_queue();
        application_query(&mut snapshot)
            .native_identity
            .as_mut()
            .unwrap()
            .qualifiers
            .insert(qualifier.into(), changed.into());
        queue_without_candidates(&snapshot);
    }
    let mut snapshot = observed_queue();
    application_query(&mut snapshot).native_identity.as_mut().unwrap().query_id = "99".into();
    queue_without_candidates(&snapshot);
    let mut snapshot = observed_queue();
    let mut duplicate = application_query(&mut snapshot).clone();
    duplicate.id = "another-display-query".into();
    snapshot.workload.as_mut().unwrap().queries.push(duplicate);
    queue_without_candidates(&snapshot);
    let mut snapshot = observed_queue();
    let duplicate = captured(&mut snapshot).observations[0].clone();
    captured(&mut snapshot).observations.push(duplicate);
    queue_without_candidates(&snapshot);
}

#[test]
fn partial_native_reads_keep_exact_present_candidates_and_source_loss_does_not_zero_fill_the_population() {
    let mut snapshot = observed_queue();
    snapshot
        .sources
        .iter_mut()
        .find(|source| source.source == culprit_core::source_id::QUERY_COMPILATION)
        .unwrap()
        .state = SourceState::Partial;
    assert_eq!(CompileQueueCheck.run(&snapshot)[0].queries.len(), 1);
    captured(&mut snapshot).observations.clear();
    queue_without_candidates(&snapshot);
    for state in [SourceState::Unavailable, SourceState::NotApplicable, SourceState::Empty] {
        let mut snapshot = observed_queue();
        snapshot
            .sources
            .iter_mut()
            .find(|source| source.source == culprit_core::source_id::QUERY_COMPILATION)
            .unwrap()
            .state = state;
        queue_without_candidates(&snapshot);
    }
    let mut snapshot = observed_queue();
    captured(&mut snapshot).observations[0].source = "another-native-source".into();
    queue_without_candidates(&snapshot);
}
