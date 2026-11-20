//! Explicit manufactured history on the existing nonce-owned disposable server, never real page corruption.
use super::*;
use crate::{
    read::{Parameter, ReadError},
    test_support::{
        owned_captures::{Captured, OwnedCaptures},
        owned_case::{require_active, until_cancelled},
    },
    value::Rec,
};
use culprit_collector::capture::CollectionCancellation;
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{collections::BTreeMap, panic::AssertUnwindSafe};

const CONTROL_QUERIES: u32 = 2;
const CONTROL_ROWS: usize = 7;
const CONTROL_BYTES: u32 = 8192;
const CONTROL_STATEMENT_MS: u32 = 1000;
const CONTROL_TOTAL_MS: u32 = 20_000;
const FIXTURE_FILE_ID: u32 = 2_147_483_647;
const SEED: &str = r#"SET /* culprit fixture */ NOCOUNT ON;
DECLARE @owned int=DB_ID(),@foreign int=DB_ID(N'master');
IF @owned<=4 OR DB_NAME()<>@P1 OR @foreign IS NULL OR @foreign=@owned THROW 50000,'Owned history target unavailable',1;
IF EXISTS(SELECT 1 FROM msdb.dbo.suspect_pages WHERE database_id=@owned)
 OR EXISTS(SELECT 1 FROM msdb.dbo.suspect_pages WHERE database_id=@foreign AND file_id=2147483647 AND page_id=7)
 THROW 50000,'Owned history fixture already populated',1;
INSERT /* culprit fixture */ msdb.dbo.suspect_pages(database_id,file_id,page_id,event_type,error_count,last_update_date)
OUTPUT inserted.database_id,inserted.file_id,inserted.page_id,inserted.event_type,inserted.error_count,
 CONVERT(varchar(30),inserted.last_update_date,126) AS native_local_update
VALUES
 (@owned,2147483647,1,4,101,CONVERT(datetime,N'2099-12-31T23:59:59.997',126)),
 (@owned,2147483647,2,5,103,CONVERT(datetime,N'2099-12-31T23:59:59.997',126)),
 (@owned,2147483647,3,7,107,CONVERT(datetime,N'2099-12-31T23:59:59.997',126)),
 (@owned,2147483647,4,1,11,CONVERT(datetime,N'2000-01-02T03:04:01.000',126)),
 (@owned,2147483647,5,2,13,CONVERT(datetime,N'2000-01-02T03:04:02.003',126)),
 (@owned,2147483647,6,3,17,CONVERT(datetime,N'2000-01-02T03:04:05.120',126)),
 (@foreign,2147483647,7,2,701,CONVERT(datetime,N'2199-12-31T23:59:59.997',126));"#;
const CLEANUP: &str = r#"SET /* culprit fixture */ NOCOUNT ON;
IF DB_ID()<>@P1 OR DB_NAME()<>@P3 THROW 50000,'Owned history target changed',1;
DELETE /* culprit fixture */ FROM msdb.dbo.suspect_pages
OUTPUT deleted.database_id,deleted.file_id,deleted.page_id,deleted.event_type,deleted.error_count,
 CONVERT(varchar(30),deleted.last_update_date,126) AS native_local_update
WHERE (database_id=@P1 AND file_id=2147483647 AND page_id BETWEEN 1 AND 6)
 OR (database_id=@P2 AND file_id=2147483647 AND page_id=7);"#;

#[derive(Clone, Debug, PartialEq, Eq)]
struct HistoryRow {
    database_id: u32,
    file_id: u32,
    page_id: u32,
    event_type: u32,
    error_count: u32,
    native_local_update: String,
}
struct Seeded {
    rows: BTreeMap<u32, HistoryRow>,
    generation: u64,
    database_id: u32,
    foreign_database_id: u32,
    active_max: String,
}
fn rows(records: &[Rec]) -> Result<BTreeMap<u32, HistoryRow>> {
    if records.len() != CONTROL_ROWS {
        return Err(invalid_data());
    }
    let number = |row: &Rec, name| u32::try_from(crate::read::integer(row, name)?).map_err(|_| invalid_data());
    let mut result = BTreeMap::new();
    for row in records {
        let value = HistoryRow {
            database_id: number(row, "database_id")?,
            file_id: number(row, "file_id")?,
            page_id: number(row, "page_id")?,
            event_type: number(row, "event_type")?,
            error_count: number(row, "error_count")?,
            native_local_update: text(row, "native_local_update")?,
        };
        if value.file_id != FIXTURE_FILE_ID || result.insert(value.page_id, value).is_some() {
            return Err(invalid_data());
        }
    }
    Ok(result)
}
fn seeded(records: &[Rec], generation: Option<u64>) -> Result<Seeded> {
    let rows = rows(records)?;
    let database_id = rows.get(&1).ok_or_else(invalid_data)?.database_id;
    let foreign_database_id = rows.get(&7).ok_or_else(invalid_data)?.database_id;
    if database_id <= 4 || foreign_database_id == 0 || foreign_database_id == database_id {
        return Err(invalid_data());
    }
    for (page_id, event_type, error_count) in
        [(1, 4, 101), (2, 5, 103), (3, 7, 107), (4, 1, 11), (5, 2, 13), (6, 3, 17), (7, 2, 701)]
    {
        let row = rows.get(&page_id).ok_or_else(invalid_data)?;
        if row.database_id != if page_id == 7 { foreign_database_id } else { database_id }
            || row.event_type != event_type
            || row.error_count != error_count
        {
            return Err(invalid_data());
        }
    }
    let active_max = rows[&6].native_local_update.clone();
    if ![1, 2, 3, 7].into_iter().all(|page| rows[&page].native_local_update > active_max)
        || ![4, 5].into_iter().all(|page| rows[&page].native_local_update < active_max)
    {
        return Err(invalid_data());
    }
    Ok(Seeded { rows, generation: generation.ok_or_else(invalid_data)?, database_id, foreign_database_id, active_max })
}
fn query<'a>(
    reader: &'a mut ReadSession,
    cancellation: &'a CollectionCancellation,
    sql: &'a str,
    parameters: Vec<Parameter>,
) -> LocalBoxFuture<'a, Result<Vec<Rec>>> {
    Box::pin(async move {
        until_cancelled(cancellation, async move {
            Box::pin(reader.query(sql, parameters, CONTROL_ROWS)).await.map_err(ReadError::into_error)
        })
        .await
    })
}

fn capture<'a>(
    originals: &'a mut OwnedCaptures,
    cancellation: &'a CollectionCancellation,
    factory: &'a Factory,
) -> LocalBoxFuture<'a, Result<Box<Captured>>> {
    Box::pin(async move {
        Box::pin(originals.capture_with_factory(
            CollectionSelection::Snapshot { dataset: SnapshotDataset::SuspectPages },
            cancellation,
            factory,
        ))
        .await
        .map(Box::new)
    })
}

fn work<'a>(
    control: &'a mut ReadSession,
    cancellation: &'a CollectionCancellation,
    fixture: &'a OwnedSourceFixture,
    factory: &'a Factory,
    originals: &'a mut OwnedCaptures,
    seed: &'a mut Option<Seeded>,
) -> LocalBoxFuture<'a, Result<Box<Captured>>> {
    Box::pin(async move {
        let inserted = query(control, cancellation, SEED, vec![Parameter::Text(fixture.database.clone())]).await?;
        *seed = Some(seeded(&inserted, control.connection_generation())?);
        let captured = capture(originals, cancellation, factory).await?;
        validate(&captured, fixture, seed.as_ref().ok_or_else(invalid_data)?)?;
        Box::pin(originals.verify_retained(&captured)).await?;
        Ok(captured)
    })
}

async fn cleanup(
    reader: &mut ReadSession,
    cancellation: &CollectionCancellation,
    seed: &Seeded,
    database: &str,
) -> Result<()> {
    // Uncertain/replaced work cannot be repaired by SQL on another session. The supervisor destroys the owned server.
    require_active(cancellation)?;
    if reader.connection_generation() != Some(seed.generation) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let deleted = query(
        reader,
        cancellation,
        CLEANUP,
        vec![
            Parameter::Integer(seed.database_id.into()),
            Parameter::Integer(seed.foreign_database_id.into()),
            Parameter::Text(database.into()),
        ],
    )
    .await?;
    if rows(&deleted)? != seed.rows {
        return Err(invalid_data());
    }
    Ok(())
}
fn validate(captured: &Captured, fixture: &OwnedSourceFixture, seed: &Seeded) -> Result<()> {
    let source = captured.source()?;
    let context = suspect_page_context(source)?;
    if context.target.database().is_none_or(|database| database.database != fixture.database)
        || context.database_writable != Some(true)
        || captured.manifest.source_queries != 11
        || captured.records.len() != 1
        || source.observation.snapshot().is_none_or(|header| header.sources[0].state != SourceState::Collected)
    {
        return Err(invalid_data());
    }
    let record = &captured.records[0].record;
    captured.reference(0)?.validate_canonical_record(source, record)?;
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::SuspectPages(point), .. }) =
        record
    else {
        return Err(invalid_data());
    };
    if !point.fits_source(source)
        || point.native_database_id != seed.database_id.to_string()
        || point.active_count() != Some(3)
        || point.retained_count() != Some(6)
        || point.last_reported_time_without_offset.as_deref() != Some(seed.active_max.as_str())
        || point.reset != culprit_core::ResetMarker::Unknown
        || !point.observed_at.ends_with('Z')
        || point.observed_at == seed.active_max
    {
        return Err(invalid_data());
    }
    Ok(())
}

pub(super) fn run<'a>(
    fixture: &'a OwnedSourceFixture,
    cancellation: &'a CollectionCancellation,
    factory: &'a Factory,
    originals: &'a mut OwnedCaptures,
) -> LocalBoxFuture<'a, Result<()>> {
    Box::pin(async move {
        // Positive nonce admission already verified the actual server and baseline DB. The intervention is supported
        // only by that disposable single-member fixture, never an AG member or production endpoint.
        if std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").is_some()
            || std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP").is_some()
        {
            return Err(invalid_data());
        }
        let budget = CollectionBudget {
            max_queries: CONTROL_QUERIES,
            max_snapshot_rows: 14,
            page_rows: 7,
            max_pages: 2,
            page_bytes: CONTROL_BYTES,
            total_bytes: CONTROL_BYTES * 2,
            statement_timeout_ms: CONTROL_STATEMENT_MS,
            total_timeout_ms: CONTROL_TOTAL_MS,
            ..Default::default()
        };
        budget.validate()?;
        let mut control = Box::new(ReadSession::new(fixture.pool.clone(), budget));
        let mut seed = None;
        let work = AssertUnwindSafe(work(&mut control, cancellation, fixture, factory, originals, &mut seed))
            .catch_unwind()
            .await;
        // No assertion or panic reporting precedes cleanup and closing the same retained control owner.
        let cleanup = match seed.as_ref() {
            Some(seed) => Box::pin(cleanup(&mut control, cancellation, seed, &fixture.database)).await,
            None => Ok(()),
        };
        let shutdown = Box::pin(control.close()).await;
        let metrics = control.read_metrics();
        let saved = crate::test_support::save_cost_receipt(
            &crate::test_support::receipt_directory(),
            "suspect-page-manufactured-history-control",
            &serde_json::json!({
                "manufactured_history_rows":true,"actual_corruption_proven":false,"native_cost_approved":false,
                "seed_completed":seed.is_some(),"cleanup_confirmed":seed.is_some()&&cleanup.is_ok(),
                "cleanup_error_code":cleanup.as_ref().err().map(|error|error.code),
                "shutdown":match shutdown {SourceShutdown::Quiescent=>"quiescent",SourceShutdown::Unconfirmed=>"unconfirmed"},
                "control_queries":control.queries(),"control_metrics":metrics,
                "supervisor_destruction_required":seed.is_none()||cleanup.is_err()||shutdown!=SourceShutdown::Quiescent,
            }),
        );
        let captured = match work {
            Ok(result) => result?,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        cleanup?;
        if shutdown != SourceShutdown::Quiescent {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if control.queries() != CONTROL_QUERIES
            || metrics.as_ref().is_none_or(|metrics| metrics.has_unconfirmed_work())
            || factory.0.source_shutdown() != SourceShutdown::Quiescent
        {
            return Err(invalid_data());
        }
        saved?;
        crate::test_support::save_cost_receipt(
            &crate::test_support::receipt_directory(),
            "suspect-page-history-original",
            &serde_json::json!({
                "manifest":captured.manifest,"original_records":captured.records,"capture_export_ms":captured.capture_export_ms,
                "manufactured_history_rows":true,"selected_rows":6,"active_rows":3,"foreign_rows":1,
                "actual_corruption_proven":false,"native_cost_approved":false,"historical_database_incarnation_proven":false,
            }),
        )?;
        Ok(())
    })
}

mod stack_tests;
