//! Owned metadata compatibility is independent of the strict, explicitly Agent-enabled history experiment.
//! Fixture job execution is separate from source work; neither experiment grants production approval.
use super::*;
use crate::{
    conn::SessionKind,
    test_support::{
        OwnedSourceFixture, config, connect,
        owned_case::{finish_case, until_cancelled},
        receipt_directory, save_cost_receipt, verify_owned_source,
    },
};
use culprit_collector::{
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::{CompletedCaptureBudget, Outbox},
};
use culprit_read::admission::SourceGate;
use futures_util::FutureExt;
use serde_json::json;
use std::{
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::Instant;
mod fixture;
use fixture::AgentJob;
const CASE_TIMEOUT: Duration = Duration::from_secs(90);
const METADATA_CASE_TIMEOUT: Duration = Duration::from_secs(30);
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const EXPECTED_SOURCE_QUERIES: u32 = 13;

#[tokio::test]
#[ignore = "requires ordinary nonce-owned loopback SQL Server, owned source env and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; creates/executes no job and permits Agent off; no history or production-cost qualification"]
async fn owned_agent_metadata_source_roundtrips_without_creating_or_executing_jobs() {
    let directory = scenario_directory("agent-jobs-metadata");
    let fixture = open_owned_fixture().await.expect("full nonce-owned loopback admission");
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + METADATA_CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(Box::pin(capture_original(&fixture, &directory, &cancellation))).catch_unwind(),
    )
    .await;
    let source_shutdown = fixture.pool.source_shutdown();
    save_cost_receipt(
        &directory,
        "agent-jobs-metadata-outcome",
        &json!({
            "completed": !expired && matches!(&outcome, Ok(Ok(_))),
            "case_deadline_reached": expired,
            "case_client_elapsed_ms": started.elapsed().as_secs_f64()*1000.0,
            "fixture_admission_client_elapsed_ms": fixture.admission_elapsed_ms,
            "source_shutdown": crate::read::measurement::shutdown_state(source_shutdown),
            "fixture_job_created_or_executed": false,
            "agent_running_required": false,
            "history_failure_success_qualified": false,
            "production_admission": "unqualified",
            "native_cpu_io_cost_qualified": false,
            "active_source_cancellation_exercised": false,
            "cleanup": "same source owner closes or fences; no fixture job exists to remove; existing supervisor owns final nonce-labelled container and volume removal",
        }),
    )
    .expect("private bounded metadata outcome receipt");
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => {
            result.expect("actual native metadata/source capture and canonical replay");
        }
    }
    assert!(!expired);
    assert_eq!(source_shutdown, SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server explicitly started with MSSQL_AGENT_ENABLED=true, owned source env and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; source remains Unqualified"]
async fn owned_agent_history_keeps_real_failure_success_and_current_definition_separate_on_the_same_reader() {
    let directory = scenario_directory("agent-jobs-history");
    let fixture = open_owned_fixture().await.expect("full nonce-owned loopback admission");
    let mut job = AgentJob::new(&fixture).expect("unique owned Agent job name");
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        Instant::now() + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(async {
            job.require_agent(&cancellation).await?;
            job.create().await?;
            let failure = job.run_and_wait(0, &cancellation).await?;
            assert_eq!(failure.1, 0, "genuine owned failure history");
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            job.make_healthy().await?;
            let healthy = job.run_and_wait(failure.0, &cancellation).await?;
            assert_eq!(healthy.1, 1, "genuine independent success history");
            let original = Box::pin(capture_original(&fixture, &directory, &cancellation)).await?;
            assert_owned_history(&original, job.id().ok_or_else(invalid_data)?);
            Ok::<_, Error>(())
        })
        .catch_unwind(),
    )
    .await;
    // A separate admitted control client can clean only this exact nonce-owned job; source owners settled above.
    let cleanup = job.cleanup().await;
    let source_shutdown = fixture.pool.source_shutdown();
    save_cost_receipt(&directory,"agent-jobs-outcome",&json!({"case_deadline_reached":expired,"case_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"fixture_control_queries":job.queries(),"owned_job_removed":cleanup.is_ok(),"source_shutdown":crate::read::measurement::shutdown_state(source_shutdown),"production_admission":"unqualified","native_cpu_io_cost_qualified":false,"active_source_cancellation_exercised":false,"fixture_work":"one exact nonce-owned Agent job with a trivial THROW followed by a trivial SELECT; all setup/start/poll/cleanup work is outside collector measurements"})).expect("private bounded outcome receipt");
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("genuine Agent compatibility capture"),
    };
    cleanup.expect("only the owned job was removed and absence confirmed");
    assert!(!expired);
    assert_eq!(source_shutdown, SourceShutdown::Quiescent);
}

fn scenario_directory(name: &str) -> PathBuf {
    let directory = receipt_directory().join(name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.recursive(false).create(&directory).expect("fresh private per-scenario receipt directory");
    directory
}

async fn open_owned_fixture() -> Result<OwnedSourceFixture> {
    // Only a dedicated read-only fixture admission connection can be interrupted here. A timeout retires it;
    // no source reader or capture lease has opened, and no quiescent native-work claim is returned.
    tokio::time::timeout(ADMISSION_TIMEOUT, OwnedSourceFixture::open("culprit-agent-jobs-source"))
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
}

struct Factory {
    pool: Arc<Pool>,
    port: u16,
}
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        source(self.pool.clone(), request, "127.0.0.1".into(), self.port)
    }
}
async fn capture_original(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<MssqlAgentJobsSnapshot> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let budget = bounded_budget(&CollectionBudget::upper_limits());
    let mut discovery = ReadSession::new(fixture.pool.clone(), budget.clone());
    let owner = until_cancelled(cancellation, crate::instance::read(&mut discovery)).await;
    let discovery_shutdown = discovery.close().await;
    if discovery_shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let (owner, _) = owner?;
    let target = CollectionTarget::Shared(owner.identity.clone());
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: owner.identity },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget,
    };
    let temporary = tempfile::tempdir().map_err(|_| invalid_data())?;
    let mut outbox = Outbox::create(&temporary.path().join("outbox"), OutboxLimits::default())?;
    let mut lease = outbox.reserve("owned-agent-fixture", &request).await?;
    let identity = lease.identity().clone();
    let output = lease.directory().to_owned();
    let result = capture::collect(
        &SourceGate::default(),
        &Factory { pool: fixture.pool.clone(), port: fixture.port },
        request,
        &output,
        identity.clone(),
        cancellation,
        Some(&mut lease),
    )
    .await;
    lease.finish().await?;
    let manifest = result?;
    if manifest.status != CollectionStatus::Complete {
        return Err(invalid_data());
    }
    assert_eq!(fixture.pool.source_shutdown(), SourceShutdown::Quiescent);
    let attempt = outbox.attempt_receipt(&identity, None).await?;
    let completed = outbox
        .delivery()
        .completed_snapshot_for_connection(
            "owned-agent-fixture",
            &attempt,
            SnapshotDataset::Jobs,
            CompletedCaptureBudget::default(),
            cancellation,
        )
        .await?;
    let original = completed.into_operational_read()?;
    let captured_rows = original.observations.len();
    let native = original.description.observation.snapshot().ok_or_else(invalid_data)?;
    assert_eq!(original.description.target, target);
    assert!(matches!(original.description.partition, SourcePartition::InstanceLocal { .. }));
    assert!(original.description.partition.is_qualified());
    assert_eq!(native.records as usize, captured_rows);
    let read = JobsSnapshot::from_source(&original.description, original.observations, true)?;
    let report = culprit_checks::jobs::explain(&read)?;
    let JobsSnapshot::MssqlAgent { snapshot: read, .. } = read else {
        return Err(invalid_data());
    };
    record_source_receipt(directory, &manifest, &read, &report, discovery.queries())?;
    assert_native_metadata(&read, &report, &target, captured_rows);
    Ok(*read)
}

fn record_source_receipt(
    directory: &Path,
    manifest: &CollectionManifest,
    read: &MssqlAgentJobsSnapshot,
    report: &culprit_core::JobsReport,
    preflight_queries: u32,
) -> Result<()> {
    let metrics = manifest.read_metrics.as_ref().ok_or_else(invalid_data)?;
    save_cost_receipt(
        directory,
        "agent-jobs-source",
        &json!({
            "source_queries": manifest.source_queries,
            "source_budget": manifest.request.budget,
            "artifact_inventory": manifest.files,
            "source_read_summary": metrics.summary(manifest.description.as_ref())?,
            "native_profile": read.metadata,
            "source_coverage": read.sources,
            "context": read.context,
            "captured_rows": read.observations.len(),
            "report": report,
            "same_reader": "dedicated registered mssql.agent-jobs FrozenSource and original capture owner",
            "preflight_queries": preflight_queries,
            "preflight_outside_source_measurement": true,
            "native_cpu_physical_storage_io_grants": "unknown; not inferred from client metrics or output ceilings",
            "active_source_cancellation": "unexercised; no substitute template is proof",
            "production_admission": "unqualified",
        }),
    )?;
    assert!(metrics.is_valid());
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    assert!(!metrics.has_unconfirmed_work());
    assert_eq!(manifest.source_queries, EXPECTED_SOURCE_QUERIES);
    assert_eq!(manifest.request.budget.max_queries, EXPECTED_SOURCE_QUERIES);
    assert_eq!(manifest.request.budget.statement_timeout_ms, 1000);
    assert_eq!(manifest.request.budget.total_timeout_ms, 5000);
    assert!(read.observations.len() <= manifest.request.budget.max_snapshot_rows as usize);
    assert!(
        manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
            <= u64::from(manifest.request.budget.total_bytes)
    );
    assert_eq!(metrics.templates.iter().map(|task| task.completed_tasks).sum::<u32>(), EXPECTED_SOURCE_QUERIES);
    Ok(())
}

fn assert_native_metadata(
    read: &MssqlAgentJobsSnapshot,
    report: &culprit_core::JobsReport,
    target: &CollectionTarget,
    captured_rows: usize,
) {
    for object in layout::OBJECTS {
        assert!(read.metadata.available(object), "actual owned native columns must qualify independently: {object:?}");
    }
    assert_eq!(read.metadata.recheck, MssqlAgentMetadataRecheck::Unchanged);
    assert_eq!(read.metadata.visibility, MssqlAgentVisibility::Sysadmin);
    assert_eq!(&read.context.target, target);
    assert_eq!(read.context.database_writable, None);
    assert_eq!(report.context, read.context);
    assert_eq!(report.native_rows as usize, captured_rows);
    assert_eq!(report.observed_rows as usize, captured_rows);
    assert!(report.read_complete);
    assert!(
        read.sources
            .iter()
            .find(|source| source.source == MSSQL_AGENT_HISTORY_SOURCE)
            .is_some_and(|source| !source.complete_scope),
        "a full bounded body never establishes full retained history"
    );
    let context = read.sources.iter().find(|source| source.source == MSSQL_AGENT_CONTEXT_SOURCE).unwrap();
    assert_eq!(context.records, 2, "both original context points remain explicit even when Agent is off");
    if read.observations.iter().any(|row| {
        matches!(&row.fact,
            OperationalFact::MssqlAgentContext(point) if point.session.is_none()
        )
    }) {
        assert!(!context.complete_scope, "missing Agent startup-session authority stays unknown");
    }
}

fn assert_owned_history(read: &MssqlAgentJobsSnapshot, owned: &str) {
    assert!(
        read.observations.iter().any(|row| matches!(&row.fact,
            OperationalFact::MssqlAgentHistory(point)
                if point.job_id == owned && point.step_id == "1" && point.run_status == 0
                    && point.stored_step_name.as_deref() == Some("owned failure") && point.sql_message_id == "51000"
        )),
        "the planted THROW must be the original recorded failure"
    );
    assert!(read.observations.iter().any(|row| matches!(&row.fact,
        OperationalFact::MssqlAgentHistory(point)
            if point.job_id == owned && point.step_id == "1" && point.run_status == 1
                && point.stored_step_name.as_deref() == Some("owned healthy")
    )));
    assert!(read.observations.iter().any(|row| matches!(&row.fact,
        OperationalFact::MssqlAgentStep(point) if point.job_id == owned && point.name == "owned healthy"
    )));
}
