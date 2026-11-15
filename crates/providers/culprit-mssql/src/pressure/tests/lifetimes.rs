use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::telemetry::{CollectionStop, SourceShutdown};

fn probe(started_at: Option<&str>) -> Probe {
    Probe {
        version: "16.0.1000.6".into(),
        major: 16,
        edition: "fixture".into(),
        engine_edition: 3,
        database: "sample".into(),
        data_bytes: None,
        can_view_database_state: true,
        can_view_server_state: true,
        can_showplan: true,
        can_view_definition: true,
        login: "fixture".into(),
        user: "fixture".into(),
        started_at: started_at.map(str::to_string),
        query_store: None,
        rds: false,
    }
}

fn native_counters(prefix: &str) -> Vec<Rec> {
    [
        ("SQL Statistics", "Batch Requests/sec", 2000, 272_696_576),
        ("SQL Statistics", "SQL Compilations/sec", 600, 272_696_576),
        ("SQL Statistics", "SQL Re-Compilations/sec", 100, 272_696_576),
        ("Buffer Manager", "Page life expectancy", 400, 65_792),
        ("General Statistics", "Logins/sec", 1000, 272_696_576),
        ("General Statistics", "Connection Reset/sec", 9000, 272_696_576),
    ]
    .into_iter()
    .map(|(family, name, value, native_type)| {
        replace(
            &crate::performance::tests::row(),
            &[
                ("native_object", Val::Text(format!("{prefix}:{family}"))),
                ("native_name", Val::Text(name.into())),
                ("native_type", Val::Int(native_type)),
                ("value", Val::Int(value)),
            ],
        )
    })
    .collect()
}

fn reader(prefix: &str) -> FakeRead {
    FakeRead::new(vec![
        Ok(vec![
            crate::waits::tests::row("THREADPOOL", 10_000),
            crate::waits::tests::row("RESOURCE_SEMAPHORE", 20_000),
        ]),
        Ok(native_counters(prefix)),
    ])
}

#[tokio::test]
async fn an_earlier_pooled_probe_cannot_supply_the_lifetime_of_the_current_native_counter_read() {
    for prefix in ["MSSQL$other-member", "SQLServer"] {
        for started_at in [Some("2000-01-01T00:00:00Z"), Some("2026-11-15T00:00:00Z"), Some("invalid"), None] {
            let mut io = reader(prefix);
            let (waits, counters) = history_from(&mut io, false).await;
            assert!(waits.as_ref().unwrap().iter().all(|wait| wait.reset == culprit_core::ResetMarker::Unknown));
            let captured = project_history(&probe(started_at), waits, counters);
            assert_eq!(captured.pressure.uptime_hours, None);
            assert!(captured.pressure.signals.iter().all(|signal| !matches!(
                signal.key.as_str(),
                culprit_core::signal_id::BATCH_REQUESTS
                    | culprit_core::signal_id::NEW_CONNECTIONS
                    | culprit_core::signal_id::THREADPOOL_WAITS
            )));
            let compiled = captured.pressure.signal(culprit_core::signal_id::COMPILES_PER_BATCH).unwrap();
            assert_eq!(compiled.value, 25.0);
            assert_eq!(compiled.status, SignalStatus::Info);
            assert_eq!(compiled.scope, SignalScope::Cumulative);
            assert!(captured.pressure.signals.iter().all(|signal| {
                signal.scope == SignalScope::Cumulative
                    || signal.key == culprit_core::signal_id::PAGE_LIFE && signal.scope == SignalScope::Now
            }));
            assert_eq!(captured.pressure.signal(culprit_core::signal_id::RECOMPILES_PER_BATCH).unwrap().value, 5.0);
            assert_eq!(captured.pressure.signal(culprit_core::signal_id::POOLED_REUSE).unwrap().value, 90.0);
            assert_eq!(captured.pressure.signal(culprit_core::signal_id::PAGE_LIFE).unwrap().value, 400.0);
            assert_eq!(captured.pressure.signal(culprit_core::signal_id::THREADPOOL_WAIT_MS).unwrap().value, 10_000.0);
            assert_eq!(captured.pressure.signal(culprit_core::signal_id::GRANT_WAIT_MS).unwrap().value, 20_000.0);
            assert!(captured.waits.iter().any(|wait| wait.wait_ms > 0.0));
            assert_eq!(io.calls.len(), 2, "qualifying lifetime cannot introduce native metadata queries");
            assert_eq!(io.closed, 1);
        }
    }
}

#[tokio::test]
async fn unknown_wait_reset_never_becomes_a_startup_lifetime_even_with_a_matching_counter_label() {
    let mut io = reader("SQLServer");
    let (waits, counters) = history_from(&mut io, false).await;
    let mut same_label = probe(Some("2000-01-01T00:00:00Z"));
    same_label.edition = "SQLServer".into();
    let captured = project_history(&same_label, waits, counters);
    assert_eq!(captured.pressure.uptime_hours, None);
    let worker = captured.pressure.signal(culprit_core::signal_id::THREADPOOL_WAIT_MS).unwrap();
    assert_eq!(worker.value, 10_000.0);
    assert_eq!(worker.status, SignalStatus::Info);
    assert_eq!(worker.scope, SignalScope::Cumulative);
    assert!(captured.pressure.signal(culprit_core::signal_id::THREADPOOL_WAITS).is_none());
}

#[tokio::test]
async fn unconfirmed_close_invalidates_both_histories_and_a_missing_counter_source_keeps_wait_positives() {
    let mut uncertain = reader("SQLServer");
    uncertain.shutdown = SourceShutdown::Unconfirmed;
    let (waits, counters) = history_from(&mut uncertain, false).await;
    assert_eq!(CollectionStop::from_error(waits.as_ref().err().unwrap()), CollectionStop::CancellationUnconfirmed);
    assert_eq!(CollectionStop::from_error(counters.as_ref().err().unwrap()), CollectionStop::CancellationUnconfirmed);
    let captured = project_history(&probe(None), waits, counters);
    assert!(captured.pressure.signals.is_empty());
    assert!(captured.waits.is_empty());
    assert_eq!(uncertain.closed, 1);

    let mut partial = FakeRead::new(vec![
        Ok(vec![crate::waits::tests::row("THREADPOOL", 1000)]),
        Err(ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied"))),
    ]);
    let (waits, counters) = history_from(&mut partial, false).await;
    let captured = project_history(&probe(Some("2000-01-01T00:00:00Z")), waits, counters);
    assert_eq!(captured.pressure.uptime_hours, None);
    assert_eq!(captured.pressure.signal(culprit_core::signal_id::THREADPOOL_WAIT_MS).unwrap().value, 1000.0);
    assert!(captured.pressure.signal(culprit_core::signal_id::COMPILES_PER_BATCH).is_none());
    assert_eq!(partial.calls.len(), 2);
    assert_eq!(partial.closed, 1);
}
