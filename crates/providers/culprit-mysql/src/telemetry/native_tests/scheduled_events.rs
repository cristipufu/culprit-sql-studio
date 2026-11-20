//! Nonce-owned event catalog acceptance. No scheduler setting change or event execution; no run-outcome proof.
use super::capture::{self, Factory};
use super::replication::required;
use crate::{
    connect::Scope,
    native,
    read::{OwnedRead, ReadSession, confirmed_result},
};
use culprit_collector::{capture::CollectionCancellation, outbox::Outbox};
use culprit_core::{Provider, Result, SourceState, telemetry::*};
use culprit_read::admission::SourceGate;
use mysql_async::Value;
use std::time::Duration;
use tokio::time::Instant;

mod fixture;
mod original;
#[cfg(test)]
mod tests;

const CASE_TIMEOUT: Duration = Duration::from_secs(90);
const SOURCE_QUERIES: u32 = 25;
const SCAN_QUERIES: u32 = 20;
use fixture::{Control, NativeEvent};

#[tokio::test]
#[ignore = "requires the nonce-owned MySQL/MariaDB smoke fixture; metadata acceptance only, never event execution or cost qualification"]
async fn owned_scheduled_events_preserve_native_catalog_visibility_privacy_and_originals_after_drop() {
    let (specification, owner) = fixture::specification();
    let factory = Factory(required(crate::MysqlProvider::new(&specification), "owned provider"));
    let limited = Factory(required(crate::MysqlProvider::new(&fixture::limited(&specification)), "limited provider"));
    let directory = tempfile::tempdir().expect("private native original receipts");
    let mut outbox = required(Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()), "outbox");
    let gate = SourceGate::default();
    let cancellation = CollectionCancellation::default();
    let mut control = Control::new(
        ReadSession::new(factory.0.connector.clone(), fixture::budget(), Scope::Database),
        owner,
        cancellation.clone(),
    );
    let deadline = Instant::now() + CASE_TIMEOUT;
    let (expired, result) = {
        let work = Box::pin(run(&factory, &limited, &mut control, &mut outbox, &gate, &cancellation));
        tokio::pin!(work);
        tokio::select! {
            value = &mut work => (Instant::now() >= deadline, value),
            _ = tokio::time::sleep_until(deadline) => {
                cancellation.cancel();
                (true, work.await)
            }
        }
    };
    if expired {
        cancellation.cancel();
    }
    // Preserve the original error. Uncertain/cancelled fixture work forbids all later SQL; the nonce supervisor
    // then destroys the container. The same reader is always awaited, including failed setup/capture/cleanup.
    let cleanup = control.cleanup().await;
    let shutdown = control.close().await;
    let accounting = control.io.read_metrics();
    eprintln!(
        "scheduled-event native: expired={expired} original_error={:?} cleanup_error={:?} shutdown={shutdown:?} control_queries={} supervisor_destruction_required={}",
        result.as_ref().err().map(|error| error.code),
        cleanup.as_ref().err().map(|error| error.code),
        control.io.query_count(),
        control.requires_supervisor_destruction(),
    );
    let result = required(result, "owned scheduled-event catalog phases");
    required(cleanup, "owned event cleanup");
    assert!(!expired);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    assert_eq!(limited.0.source_shutdown(), SourceShutdown::Quiescent);
    let accounting = accounting.expect("paid fixture native accounting");
    assert!(accounting.is_valid() && !accounting.has_unconfirmed_work());
    assert_eq!(accounting.active_tasks, 0);
    assert_eq!(accounting.untracked_tasks, 0);
    assert!(control.io.query_count() <= fixture::MAX_QUERIES);
    assert_eq!(control.io.query_count(), accounting.templates.iter().map(|query| query.completed_tasks).sum::<u32>());
    assert!(accounting.templates.iter().all(|query| query.failures == 0 && query.timeouts == 0));
    assert!(!control.requires_supervisor_destruction());

    let empty = result.empty.replay();
    let disabled = result.disabled.replay();
    let enabled = result.enabled.replay();
    let dropped = result.dropped.replay();
    for captured in [&empty, &disabled, &enabled, &dropped] {
        assert_eq!(captured.manifest.source_queries, SOURCE_QUERIES);
        assert_eq!(captured.manifest.status, CollectionStatus::Complete);
        assert_eq!(captured.manifest.request.budget, request().budget);
        let read = snapshot(captured);
        assert!(read.is_valid_read(true));
        assert!(read.context.has_qualified_local_owner());
        assert!(read.metadata.database_event_visibility);
        assert_eq!(read.metadata.layout, MysqlEventCatalogLayout::Available);
        assert_eq!(read.metadata.scheduler_setting, Some(result.disabled_native.scheduler));
        assert_eq!(read.context.database_writable, Some(true));
        result.disabled_native.assert_private(captured);
    }
    for captured in [&empty, &dropped] {
        let read = snapshot(captured);
        assert!(read.observations.is_empty());
        assert_eq!(read.sources[0].records, 0);
        assert_eq!(read.sources[0].state, SourceState::Empty);
    }
    assert_event(&disabled, &result.disabled_native, MysqlEventStatus::Disabled);
    assert_event(&enabled, &result.enabled_native, MysqlEventStatus::Enabled);
    assert_ne!(disabled.manifest.capture, enabled.manifest.capture);
    assert_ne!(empty.manifest.capture, dropped.manifest.capture);
    assert_eq!(result.disabled_native.execute_at, result.enabled_native.execute_at);
    assert_eq!(result.scan.observations.len(), 1);
    assert!(result.scan.is_valid_read(true));
    let OperationalFact::MysqlScheduledEvent(scanned) = &result.scan.observations[0].fact else {
        panic!("same native buffered event acquisition");
    };
    result.disabled_native.assert_event(scanned, MysqlEventStatus::Disabled);
    assert_limited(&result.limited);
    let recipe = super::super::recipes().into_iter().find(|recipe| recipe.selector.matches(&request())).unwrap();
    assert_eq!(recipe.id, "mysql.jobs");
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    eprintln!(
        "scheduled-event native: four authenticated root captures x25 source queries, one same-reader buffered acquisition x20, one original limited-reader attempt; disabled/future-enabled catalog points only, no event invocation, native run outcome or production cost qualification"
    );
}

struct Accepted {
    empty: original::Original,
    disabled: original::Original,
    enabled: original::Original,
    dropped: original::Original,
    limited: original::Original,
    disabled_native: NativeEvent,
    enabled_native: NativeEvent,
    scan: MysqlEventsSnapshot,
}
async fn run(
    factory: &Factory,
    limited: &Factory,
    control: &mut Control,
    outbox: &mut Outbox,
    gate: &SourceGate,
    cancellation: &CollectionCancellation,
) -> Result<Box<Accepted>> {
    control.admit().await?;
    let empty = Box::pin(original::take(factory, outbox, gate, cancellation)).await?;
    empty.require_root_rows(0)?;
    control.create().await?;
    let disabled_native = control.native_event().await?;
    disabled_native.require_status("DISABLED")?;
    let disabled = Box::pin(original::take(factory, outbox, gate, cancellation)).await?;
    disabled.require_root_rows(1)?;
    let scan = Box::pin(buffered(factory, cancellation)).await?;
    control.enable_future().await?;
    let enabled_native = control.native_event().await?;
    enabled_native.require_status("ENABLED")?;
    let enabled = Box::pin(original::take(factory, outbox, gate, cancellation)).await?;
    enabled.require_root_rows(1)?;
    let limited = Box::pin(original::take(limited, outbox, gate, cancellation)).await?;
    control.drop_event().await?;
    let dropped = Box::pin(original::take(factory, outbox, gate, cancellation)).await?;
    dropped.require_root_rows(0)?;
    // All four immutable source bodies are reverified after the owned catalog entry has disappeared. This is
    // original evidence, never a new source lookup or a name-only reconstruction from the present server.
    for original in [&empty, &disabled, &enabled, &dropped] {
        original.verify(outbox).await?;
    }
    limited.verify_original()?;
    Ok(Box::new(Accepted { empty, disabled, enabled, dropped, limited, disabled_native, enabled_native, scan }))
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: crate::jobs::bounded_budget(&CollectionBudget { page_rows: 1, ..Default::default() }),
    }
}
async fn buffered(factory: &Factory, cancellation: &CollectionCancellation) -> Result<MysqlEventsSnapshot> {
    let budget = request().budget;
    let mut reader = ReadSession::new(factory.0.connector.clone(), budget.clone(), Scope::Server);
    let result = {
        let work = Box::pin(native::scheduled_events::capture(&mut reader, &factory.0.connector, &budget));
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(CollectionStop::Cancelled.error()),
            value = &mut work => value.map(|captured| captured.snapshot),
        }
    };
    let result = confirmed_result(&mut reader, result).await;
    let metrics = reader.read_metrics().ok_or_else(native::invalid)?;
    let snapshot = result?;
    if reader.query_count() != SCAN_QUERIES || !metrics.is_valid() || metrics.has_unconfirmed_work() {
        return Err(native::invalid());
    }
    Ok(snapshot)
}
fn snapshot(captured: &capture::Capture) -> MysqlEventsSnapshot {
    let observations = captured
        .records
        .iter()
        .map(|row| match &row.record {
            TelemetryRecord::Operational(row) => row.clone(),
            _ => panic!("native event operational stream"),
        })
        .collect();
    required(
        MysqlEventsSnapshot::from_source(captured.manifest.description.as_ref().unwrap(), observations),
        "original event replay",
    )
}
fn assert_event(captured: &capture::Capture, expected: &NativeEvent, status: MysqlEventStatus) {
    let read = snapshot(captured);
    assert_eq!(read.observations.len(), 1);
    assert_eq!(read.sources[0].records, 1);
    let OperationalFact::MysqlScheduledEvent(event) = &read.observations[0].fact else { panic!("native event") };
    expected.assert_event(event, status);
}
fn assert_limited(original: &original::Original) {
    let captured = original.replay();
    assert!(captured.records.is_empty(), "another account's event must not become visible or a healthy zero");
    match original.error {
        Some(code) => {
            assert_eq!(code, culprit_core::ErrorCode::PermissionDenied);
            assert_eq!(captured.manifest.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable));
            assert!(captured.manifest.description.is_none());
        }
        None => {
            assert_eq!(captured.manifest.status, CollectionStatus::Complete);
            let read = snapshot(&captured);
            assert!(!read.metadata.database_event_visibility);
            assert!(!read.sources[0].complete_scope);
            match read.sources[0].state {
                SourceState::Partial => {
                    assert_eq!(read.metadata.layout, MysqlEventCatalogLayout::Available);
                    assert_eq!(read.sources[0].error_code, None);
                }
                SourceState::Unavailable => {
                    assert_eq!(read.sources[0].error_code, Some(culprit_core::ErrorCode::PermissionDenied));
                }
                _ => panic!("missing EVENT visibility cannot certify an empty or complete population"),
            }
        }
    }
}
