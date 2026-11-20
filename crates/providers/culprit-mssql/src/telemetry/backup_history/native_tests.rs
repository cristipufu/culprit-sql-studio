//! One explicitly nonce-owned backup proves the real registered source. No production backup or media access.
use super::*;
use crate::{
    read::Parameter,
    test_support::{
        OwnedSourceFixture,
        owned_captures::{Captured, OwnedCaptures},
        owned_case::{finish_case, until_cancelled},
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::SourceState;
use std::time::Duration;
use tokio::time::Instant;

const CASE_TIMEOUT: Duration = Duration::from_secs(90);
const FIXTURE_COMMAND_STATEMENT_MS: u32 = 30_000;
const FIXTURE_COMMAND_TOTAL_MS: u32 = 35_000;

#[tokio::test]
#[ignore = "requires the nonce-owned disposable loopback SQL Server fixture; one COPY_ONLY backup, not cost qualification"]
async fn owned_copy_only_backup_is_original_registered_history_and_empty_does_not_mean_failed_backups() {
    let fixture = OwnedSourceFixture::open("culprit").await.unwrap();
    // This intervention belongs to the disposable single-member supervisor, never its optional AG route.
    assert!(std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").is_none());
    assert!(std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP").is_none());
    let mut captures = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port).unwrap();
    let cancellation = CollectionCancellation::default();
    let (expired, result) = finish_case(Instant::now() + CASE_TIMEOUT, &cancellation, async {
        let empty = capture(&mut captures, &cancellation).await?;
        // Refuse an existing history before issuing any fixture backup. A second run needs a new owned supervisor.
        let (scope, header) = native_header(&empty)?;
        if scope.profile != MssqlBackupHistoryProfile::SqlServerMsdb
            || header.records != 0
            || !empty.records.is_empty()
            || header.sources[0].state != SourceState::Partial
            || header.sources[0].error_code.is_some()
            || backup_history_context(empty.source()?)?.database_writable != Some(true)
            || empty.source()?.target.database().is_none_or(|database| database.database != fixture.database)
        {
            return Err(invalid_data());
        }
        let command = backup_once(&fixture, &cancellation).await?;
        let populated = capture(&mut captures, &cancellation).await?;
        let later = capture(&mut captures, &cancellation).await?;
        captures.verify_retained(&empty).await?;
        captures.verify_retained(&populated).await?;
        captures.verify_retained(&later).await?;
        Ok::<_, culprit_core::Error>((empty, populated, later, command))
    })
    .await;
    // All original source/fixture command owners have already settled before assertions or panic reporting.
    let (empty, populated, later, command) =
        result.expect("owned completed backup metadata and immutable Outbox originals");
    assert!(!expired);
    let (original_scope, empty_header) = native_header(&empty).unwrap();
    assert_eq!(empty_header.records, 0);
    assert_eq!(empty_header.sources[0].state, SourceState::Partial);
    let (scope, header) = native_header(&populated).unwrap();
    assert_eq!(scope, original_scope);
    assert_eq!(header.records, 1);
    assert_eq!(populated.records.len(), 1);
    let TelemetryRecord::Operational(observation) = &populated.records[0].record else {
        panic!("original native backup point")
    };
    let OperationalFact::MssqlBackupHistory(point) = &observation.fact else {
        panic!("MSDB native backup observation")
    };
    assert!(point.fits_source(populated.source().unwrap()));
    assert_eq!(point.scope, *scope);
    let MssqlBackupRecord::Msdb(record) = &point.record else { panic!("native completed backup set") };
    assert_eq!(
        tiberius::Uuid::parse_str(&record.database_guid).unwrap(),
        tiberius::Uuid::parse_str(&scope.native_database.qualifiers["database_guid"]).unwrap()
    );
    assert_eq!(record.database_name, fixture.database);
    assert_eq!(record.backup_type, Some(MssqlBackupType::Database));
    assert_eq!(record.flags.copy_only, Some(true));
    assert_eq!(record.flags.checksums, Some(true));
    assert_eq!(record.flags.damaged, Some(false));
    assert_eq!(record.flags.snapshot, Some(false));
    assert!(record.started_without_offset.is_some());
    assert!(record.finished_without_offset.is_some());
    assert!(record.time_zone_quarter_hours.is_some_and(|zone| (-48..=48).contains(&zone) || zone == 127));
    assert!(record.size_bytes.as_deref().is_some_and(|bytes| bytes != "0"));
    assert!(record.lsns.first.is_some());
    assert!(record.lsns.last.is_some());
    assert!(record.initiator_user_name.is_some());
    assert_eq!(record.software.major, Some(16));
    assert_eq!(record.software.vendor_id, Some(4608));
    assert_eq!(native_header(&later).unwrap().0, scope);
    let TelemetryRecord::Operational(later_row) = &later.records[0].record else {
        panic!("later original native point")
    };
    let OperationalFact::MssqlBackupHistory(later_point) = &later_row.fact else { panic!("later backup point") };
    assert_eq!(point.record, later_point.record);
    assert_ne!(populated.manifest.capture, later.manifest.capture);
    for capture in [&empty, &populated, &later] {
        assert_eq!(capture.manifest.source_queries, 12);
        assert_eq!(capture.manifest.status, CollectionStatus::Complete);
        let (_, header) = native_header(capture).unwrap();
        assert!(!header.sources[0].complete_scope);
        assert_eq!(header.sources[0].error_code, None);
    }
    assert_eq!(command.templates.iter().map(|template| template.completed_tasks).sum::<u32>(), 1);
    assert_eq!(command.active_tasks, 0);
    assert_eq!(command.untracked_tasks, 0);
    assert!(!command.has_unconfirmed_work());
}
async fn capture(captures: &mut OwnedCaptures, cancellation: &CollectionCancellation) -> Result<Captured> {
    captures
        .capture_with_cancellation(
            CollectionSelection::Snapshot { dataset: SnapshotDataset::BackupHistory },
            cancellation,
        )
        .await
}
fn native_header(capture: &Captured) -> Result<(&MssqlBackupHistoryScope, &StateSnapshotDescription)> {
    let source = capture.source()?;
    backup_history_context(source)?;
    let SourceObservation::BackupHistory { scope, snapshot, .. } = &source.observation else {
        return Err(invalid_data());
    };
    Ok((scope, snapshot))
}
async fn backup_once(fixture: &OwnedSourceFixture, cancellation: &CollectionCancellation) -> Result<ReadWorkMetrics> {
    // This separately paid fixture command is authorized only after positive nonce admission. The native NEWID
    // names a fresh artifact inside the existing container data directory; it supplies no read/capture identity.
    let sql = format!(
        r#"DECLARE /* culprit fixture */ @owned_backup_path nvarchar(260)=
 N'/var/opt/mssql/data/'+DB_NAME()+N'_backup_history_'+CONVERT(nvarchar(36),NEWID())+N'.bak';
BACKUP DATABASE [{}] TO DISK=@owned_backup_path WITH COPY_ONLY,CHECKSUM,NO_COMPRESSION"#,
        fixture.database
    );
    let budget = CollectionBudget {
        max_queries: 1,
        max_snapshot_rows: 1,
        page_rows: 1,
        page_bytes: 4096,
        total_bytes: 4096,
        statement_timeout_ms: FIXTURE_COMMAND_STATEMENT_MS,
        total_timeout_ms: FIXTURE_COMMAND_TOTAL_MS,
        ..Default::default()
    };
    budget.validate()?;
    let mut command = ReadSession::new(fixture.pool.clone(), budget);
    let result = until_cancelled(cancellation, async {
        command.query(&sql, Vec::<Parameter>::new(), 1).await.map_err(ReadError::into_error)
    })
    .await;
    let shutdown = command.close().await;
    let metrics = command.read_metrics();
    let rows = result?;
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let metrics = metrics.ok_or_else(invalid_data)?;
    if !rows.is_empty() || command.queries() != 1 || metrics.has_unconfirmed_work() {
        return Err(invalid_data());
    }
    Ok(metrics)
}
