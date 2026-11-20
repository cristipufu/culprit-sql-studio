//! Independent controls: a snapshot's presence is not proof of retained bytes, and completed session
//! allocation counters do not become current-task or version-retention evidence.
use super::*;
use culprit_core::{CleanupMechanism, SourceState, TaskSpaceReport};

// These are fixture-only, completed-task session totals. The production TaskSpace reader remains unchanged.
// https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-session-space-usage-transact-sql
const COMPLETED_SESSION_SPACE: &str = r#"SELECT /* culprit fixture */
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 CONVERT(varchar(16),s.session_id) AS session_id,
 CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 su.database_id,su.user_objects_alloc_page_count,su.user_objects_dealloc_page_count,
 su.internal_objects_alloc_page_count,su.internal_objects_dealloc_page_count,
 CASE WHEN OBJECT_ID(N'tempdb..#cleanup_ordinary_temp') IS NOT NULL THEN 1 ELSE 0 END AS local_object_present
 FROM sys.dm_db_session_space_usage su
 JOIN sys.dm_exec_sessions s ON s.session_id=su.session_id
 WHERE su.session_id=@@SPID AND su.database_id=2"#;

async fn read_only_snapshot(
    fixture: &mut Fixture,
    cancellation: &CollectionCancellation,
) -> culprit_core::Result<serde_json::Value> {
    check_cancellation(cancellation)?;
    fixture.setup().await?;
    check_cancellation(cancellation)?;
    // The completed seed is the only write to the owned application table. This control starts no
    // generator and makes no claim that independent metadata/internal version activity must be zero.
    // Match the existing native read-only transaction control: finish deferred compilation/statistics
    // work before beginning the transaction whose zero log count is under observation.
    fixture.protector_batch("SELECT n FROM dbo.cleanup_fixture WHERE id=255").await?;
    fixture
        .protector_batch(
            "SET TRANSACTION ISOLATION LEVEL SNAPSHOT;BEGIN TRAN;SELECT n FROM dbo.cleanup_fixture WHERE id=255",
        )
        .await?;
    fixture.fixture_queries += 1;
    let actor = Actor::read(fixture.protector.as_mut().ok_or_else(invalid_data)?).await?;
    let mut captures = fixture.captures()?;
    let original = capture(&mut captures, cancellation).await?;
    fixture.retain_stage("snapshot-without-generator", &original);
    let native = horizon(&original).oldest_confirmed.as_ref().expect("original read-only snapshot");
    assert!(actor.matches(native));
    assert_eq!(native.snapshot_isolation, Some(true));
    assert_eq!(native.log_record_count.as_deref(), Some("0"));
    let original_report = report(&original)?;
    assert_candidate_only(&original_report);
    let observed_versions = version_kibibytes(&original);
    fixture.protector_batch("COMMIT").await?;
    let after = capture(&mut captures, cancellation).await?;
    fixture.retain_stage("read-only-snapshot-ended", &after);
    assert!(horizon(&after).oldest_confirmed.is_none());
    assert!(horizon(&after).oldest_unattributed_versioning.is_none());
    assert_same_source(&original, &after)?;
    let after_report = report(&after)?;
    assert!(after_report.entries.iter().all(|entry| entry.mechanisms.is_empty()));
    captures.verify_retained(&original).await?;
    captures.verify_retained(&after).await?;
    Ok(json!({
        "reports":[original_report,after_report],
        "post_setup_generator_updates":0,
        "observed_version_kibibytes":observed_versions,
        "observed_version_gauge_was_zero":observed_versions==0,
        "version_gauge_zero_required":false,
        "snapshot_establishes_retention_or_pressure":false,
        "source_dispatches":22,
        "source_session_setup_dispatches":1,
        "native_session_work_is_partial_self_observation":true,
        "cost_qualified":false,
    }))
}

async fn ordinary_temp_allocation(
    fixture: &mut Fixture,
    cancellation: &CollectionCancellation,
) -> culprit_core::Result<serde_json::Value> {
    check_cancellation(cancellation)?;
    fixture.setup().await?;
    check_cancellation(cancellation)?;
    // The allocation request is fully drained. The original session keeps this local object alive
    // while the canonical readers observe their own different, later populations.
    fixture.generator_batch("CREATE TABLE #cleanup_ordinary_temp(id int NOT NULL,pad char(512) NOT NULL);INSERT #cleanup_ordinary_temp SELECT id,pad FROM dbo.cleanup_fixture;IF @@ROWCOUNT<>256 THROW 51000,'ordinary temp quota incomplete',1").await?;
    fixture.fixture_queries += 1;
    let rows = crate::conn::query(fixture.generator.as_mut().ok_or_else(invalid_data)?, COMPLETED_SESSION_SPACE)
        .await
        .map_err(crate::conn::map_error)?;
    let [native] = rows.as_slice() else { return Err(invalid_data()) };
    let session = NativeActivityIdentity {
        id: text(native, "session_id")?,
        generation: crate::sql_time::native_time(&text(native, "session_generation")?)?,
    };
    assert!(session.is_valid());
    assert_eq!(crate::read::integer(native, "database_id")?, 2);
    assert!(crate::read::boolean(native, "local_object_present")?);
    let allocated = crate::read::optional_counter(native, "user_objects_alloc_page_count")?
        .expect("known native completed-session user allocation");
    assert!(allocated.parse::<u64>().is_ok_and(|pages| pages > 0));
    let observed_at = canonical_time(&text(native, "observed_at")?)?;
    let completed_session = json!({
        "session":session,
        "observed_at":observed_at,
        "native_database_id":2,
        "user_allocated_pages":allocated,
        "user_deallocated_pages":crate::read::optional_counter(native,"user_objects_dealloc_page_count")?,
        "internal_allocated_pages":crate::read::optional_counter(native,"internal_objects_alloc_page_count")?,
        "internal_deallocated_pages":crate::read::optional_counter(native,"internal_objects_dealloc_page_count")?,
        "local_object_present":true,
        "population":"completed-task-session-totals",
        "footprint_or_allocating_statement_established":false,
    });
    let mut captures = fixture.captures()?;
    let cleanup = capture(&mut captures, cancellation).await?;
    fixture.retain_stage("ordinary-temp-without-retention", &cleanup);
    assert!(horizon(&cleanup).oldest_confirmed.is_none());
    assert!(horizon(&cleanup).oldest_unattributed_versioning.is_none());
    let cleanup_report = report(&cleanup)?;
    assert!(cleanup_report.entries.iter().all(|entry| entry.mechanisms.is_empty()));
    let task_capture = captures
        .capture_with_cancellation(CollectionSelection::Snapshot { dataset: SnapshotDataset::TaskSpace }, cancellation)
        .await?;
    fixture.retain_stage("later-task-population", &task_capture);
    assert_eq!(task_capture.manifest.status, CollectionStatus::Complete);
    // Idle, completed work cannot acquire a live request generation or a current statement. Orphan
    // native task rows may remain; their counters are retained without borrowing the session oracle.
    let tasks = task_capture
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.record else { panic!("native task row") };
            let OperationalFact::TaskSpace(task) = &row.fact else { panic!("native task population") };
            assert!(task.request.is_none());
            task.as_ref()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        task_capture.manifest.source_queries, 10,
        "without current requests there are no participant/detail reads"
    );
    let task_report = task_report(&task_capture, &tasks)?;
    assert!(task_report.entries.iter().all(|entry| entry.current_request.is_none()));
    assert_same_source(&cleanup, &task_capture)?;
    let observed_versions = version_kibibytes(&cleanup);
    fixture.generator_batch("DROP TABLE #cleanup_ordinary_temp").await?;
    captures.verify_retained(&cleanup).await?;
    captures.verify_retained(&task_capture).await?;
    Ok(json!({
        "completed_session_counter_oracle":completed_session,
        "cleanup_report":cleanup_report,
        "task_report":task_report,
        "observed_version_kibibytes":observed_versions,
        "version_gauge_zero_required":false,
        "session_counters_lent_to_tasks":false,
        "allocation_establishes_retention_pressure_or_cause":false,
        "source_dispatches":21,
        "source_session_setup_dispatches":1,
        "native_session_work_is_partial_self_observation":true,
        "cost_qualified":false,
    }))
}

fn assert_candidate_only(report: &CleanupReport) {
    let [entry] = report.entries.as_slice() else { panic!("one native snapshot candidate") };
    assert_eq!(entry.mechanisms, vec![CleanupMechanism::ConfirmedDatabaseTransaction]);
    assert!(entry.explanation.contains(&culprit_core::t!("view.cleanup.transaction.context.snapshot_zero_log")));
    assert!(entry.investigation.contains(&culprit_core::t!("view.cleanup.transaction.investigate_snapshot")));
}

fn task_report(captured: &Captured, tasks: &[&TaskSpaceObservation]) -> culprit_core::Result<TaskSpaceReport> {
    let native = captured.source()?.observation.snapshot().ok_or_else(invalid_data)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid_data()) };
    assert!(matches!(coverage.state, SourceState::Empty | SourceState::Partial | SourceState::Collected));
    let source =
        SourceOutcome { source: coverage.source.clone(), state: coverage.state, reason: None, reason_message: None };
    culprit_checks::task_space::explain(tasks, Some(&source))
}

#[tokio::test]
#[ignore = "requires guarded nonce-owned SQL Server and private receipt directory"]
async fn owned_read_only_snapshot_remains_a_candidate_without_a_version_generation_workload() {
    run_owned_case(
        "cleanup-read-only-snapshot-control",
        ExpectedWork { fixture_queries: 9, session_setups: 4, source_queries: 22, captures: 2 },
        read_only_snapshot,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires guarded nonce-owned SQL Server and private receipt directory"]
async fn owned_ordinary_temp_allocation_is_not_a_version_retainer_or_a_current_task_owner() {
    run_owned_case(
        "cleanup-ordinary-temp-control",
        ExpectedWork { fixture_queries: 8, session_setups: 4, source_queries: 21, captures: 2 },
        ordinary_temp_allocation,
    )
    .await;
}
