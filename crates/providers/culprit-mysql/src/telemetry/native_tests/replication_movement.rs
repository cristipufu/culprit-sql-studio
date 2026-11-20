//! Advancing native coordinates in one owned MySQL8.4 topology. This is not a lag, pressure or cost verdict.
use super::{
    capture,
    replication::{counter, required, valid_owner},
};
use crate::connect::Flavor;
use culprit_collector::capture::{CollectionCancellation, collect_once};
use culprit_core::{Provider, telemetry::*};
use culprit_read::admission::SourceGate;
use fixture::Topology;
use std::{
    path::Path,
    time::{Duration, Instant},
};

mod fixture;

const MAX_OBSERVATIONS: usize = 20;
const MOVEMENT_WINDOW: Duration = Duration::from_secs(20);

#[tokio::test]
#[ignore = "requires the optional nonce-owned MySQL8.4 source/replica pair; run through the smoke supervisor"]
async fn moving_replication_preserves_native_source_and_file_coordinates_through_actual_collector_reads() {
    let topology = Topology::load();
    let (source, version) = topology.guard().await;
    let factory = capture::Factory(topology.replica());
    let directory = tempfile::tempdir().expect("private moving replication artifacts");
    let before = observe(&factory, &topology, &version, &directory.path().join("before"), budget()).await;
    topology.write(source).await;
    let deadline = Instant::now() + MOVEMENT_WINDOW;
    for attempt in 0..MAX_OBSERVATIONS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut budget = budget();
        budget.total_timeout_ms = remaining.as_millis().clamp(1, 30_000) as u32;
        budget.statement_timeout_ms = budget.statement_timeout_ms.min(budget.total_timeout_ms);
        let after =
            observe(&factory, &topology, &version, &directory.path().join(format!("after-{attempt}")), budget).await;
        assert_eq!(
            after.received_file, before.received_file,
            "native positions from different files cannot be compared"
        );
        assert_eq!(
            after.executed_file, before.executed_file,
            "native apply positions from different files cannot be compared"
        );
        if after.received > before.received && after.executed > before.executed {
            eprintln!(
                "moving MySQL replication conformance: unchanged native source UUID and filenames; receive/apply positions advanced after one 32-row owned write; observed={} individual complete SHOW captures; no inferred duration, byte backlog, root cause or production cost qualification",
                attempt + 2
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    panic!(
        "the owned source write did not produce observed receive and apply advancement inside the bounded fixture window"
    );
}

struct Coordinates {
    received_file: String,
    executed_file: String,
    received: u64,
    executed: u64,
}

async fn observe(
    factory: &capture::Factory,
    topology: &Topology,
    version: &str,
    output: &Path,
    budget: CollectionBudget,
) -> Coordinates {
    let result = collect_once(
        &SourceGate::default(),
        factory,
        super::replication::capture::request(budget.clone()),
        output,
        &CollectionCancellation::default(),
    )
    .await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = required(result, "moving native replication capture");
    assert_eq!(
        manifest.status,
        CollectionStatus::Complete,
        "failed native points cannot become empty or be retried as movement"
    );
    let captured = capture::read_capture(manifest, &budget, output);
    let source = captured.manifest.description.as_ref().expect("original native source");
    assert_eq!(source.partition, SourcePartition::Target);
    assert_eq!(source.continuity, SourceContinuity::Unverified);
    let samples = super::replication::capture::samples(&captured.records);
    assert_eq!(samples.len(), 1, "one exact owned channel; never select a first channel");
    let point = samples[0];
    let properties = &point.measurements.properties;
    assert_eq!(properties.get("channel"), Some(&topology.channel));
    assert_eq!(properties.get("source_host"), Some(&topology.source_host));
    assert_eq!(properties.get("source_uuid"), Some(&topology.source_uuid));
    assert_eq!(properties.get("io_running").map(String::as_str), Some("Yes"));
    assert_eq!(properties.get("sql_running").map(String::as_str), Some("Yes"));
    assert_eq!(counter(point, "io-error-code"), Some("0"));
    assert_eq!(counter(point, "sql-error-code"), Some("0"));
    assert_eq!(counter(point, "source-server-id"), Some("101"));
    let show = super::replication::capture::show_work(
        captured.manifest.read_metrics.as_ref().unwrap(),
        Flavor::Mysql,
        version,
    );
    assert_eq!(show.failures, 0);
    assert_eq!(show.rows_decoded, "1");
    Coordinates {
        received_file: properties.get("source_log_file").cloned().expect("original received source filename"),
        executed_file: properties.get("relay_source_log_file").cloned().expect("original applied source filename"),
        received: counter(point, "received-source-log-position")
            .expect("native received position")
            .parse()
            .expect("exact native u64 position"),
        executed: counter(point, "executed-source-log-position")
            .expect("native applied position")
            .parse()
            .expect("exact native u64 position"),
    }
}

fn budget() -> CollectionBudget {
    CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        max_queries: 32,
        total_timeout_ms: 30_000,
        ..Default::default()
    }
}
