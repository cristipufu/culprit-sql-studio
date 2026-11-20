//! Owned qualification of the shared table SQL/decoder; the Azure-only telemetry factory is NOT bypassed.
use super::*;
use crate::{conn::Pool, read::ReadSession, test_support::OwnedSourceFixture, value::Val};
use futures_util::FutureExt;
use std::{collections::BTreeMap, panic::AssertUnwindSafe, sync::Arc};

mod fixture;
use fixture::Fixture;

type Tables = BTreeMap<String, MssqlTableFullText>;

#[derive(Clone, Copy)]
enum ReadMode {
    Scan,
    Stream,
}
impl ReadMode {
    fn queries(self) -> u32 {
        match self {
            Self::Scan => 2,
            Self::Stream => 1,
        }
    }
}

#[tokio::test]
#[ignore = "requires the full nonce-owned loopback SQL fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD; no native FTS installation required"]
async fn owned_full_text_catalog_keeps_no_index_and_denied_null_fields_without_losing_ordinary_tables() {
    run(false).await;
}

#[tokio::test]
#[ignore = "requires the full nonce-owned loopback SQL fixture with native IsFullTextInstalled=1; unsupported is a failure, never a skipped positive"]
async fn owned_full_text_enabled_and_disabled_rows_match_scan_and_stream_without_starting_a_population() {
    run(true).await;
}

async fn run(indexed: bool) {
    let owned = OwnedSourceFixture::open("culprit-full-text-fixture").await.unwrap();
    let mut fixture = Fixture::new(&owned);
    let body = AssertUnwindSafe(async {
        fixture.initialize(indexed).await?;
        let enabled = compare_read_modes(&fixture, false).await;
        fixture.assert_available(&enabled, indexed.then_some(true));
        let denied = compare_read_modes(&fixture, true).await;
        fixture.assert_denied(&denied);
        if indexed {
            fixture.disable().await?;
            let disabled = compare_read_modes(&fixture, false).await;
            fixture.assert_available(&disabled, Some(false));
            let name = fixture.indexed_table();
            let before = enabled[&name].index.as_ref().unwrap();
            let after = disabled[&name].index.as_ref().unwrap();
            assert_eq!(after.unique_index_id, before.unique_index_id);
            assert_eq!(after.catalog_id, before.catalog_id);
            assert_eq!(after.change_tracking, before.change_tracking);
        }
        Ok::<(), culprit_core::Error>(())
    })
    .catch_unwind()
    .await;
    // A fresh verified fixture actor owns cleanup after every failure. Source readers close before unwinding here.
    let cleanup = AssertUnwindSafe(fixture.cleanup()).catch_unwind().await;
    match body {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(error)) => panic!("owned full-text acceptance failed: {error:?}"),
        Ok(Ok(())) => {}
    }
    cleanup.expect("fixture cleanup completed").expect("only exact owned objects were removed");
}

async fn compare_read_modes(fixture: &Fixture<'_>, denied: bool) -> Tables {
    let scan = read_tables(fixture, ReadMode::Scan, denied).await;
    let stream = read_tables(fixture, ReadMode::Stream, denied).await;
    assert_eq!(scan, stream, "the shared native query and decoder must agree, including unavailable slots");
    scan
}

async fn read_tables(fixture: &Fixture<'_>, mode: ReadMode, denied: bool) -> Tables {
    // Each phase has its own pool. A failed impersonation/REVERT can never seed another phase's principal.
    let pool = Arc::new(Pool::new(fixture.config(), None));
    let mut reader = ReadSession::application(pool.clone());
    let work = AssertUnwindSafe(async {
        if denied {
            reader.query(&fixture.impersonate(), vec![], 1).await.map_err(ReadError::into_error)?;
        }
        let before = reader.queries();
        let tables = match mode {
            ReadMode::Scan => {
                let result = application(&mut reader, !denied).await?;
                assert!(result.complete_scope, "the fixture withholds only SELECT on the optional full-text catalog");
                result
                    .tables
                    .into_iter()
                    .filter(|table| fixture.owns(&table.schema, &table.name))
                    .map(|table| {
                        assert_rowstore_configuration(
                            table.mssql_storage_configuration.as_ref().expect("canonical native configuration"),
                        );
                        (table.name, table.mssql_full_text.expect("canonical MSSQL projection"))
                    })
                    .collect::<Vec<_>>()
            }
            ReadMode::Stream => stream_tables(&mut reader, fixture, denied).await?,
        };
        assert_eq!(reader.queries() - before, mode.queries(), "no additional source dispatch or per-table read");
        if denied {
            reader.query("REVERT", vec![], 1).await.map_err(ReadError::into_error)?;
        }
        assert_eq!(
            reader.queries(),
            mode.queries() + if denied { 2 } else { 0 },
            "fixture context calls remain accounted"
        );
        Ok::<_, culprit_core::Error>(tables)
    })
    .catch_unwind()
    .await;
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics();
    drop(reader);
    drop(pool);
    let rows = match work {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("native shared table source, with no skipped or repaired result"),
    };
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    let metrics = metrics.unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert!(!metrics.has_unconfirmed_work());
    let mut tables = BTreeMap::new();
    for (name, state) in rows {
        assert!(
            tables.insert(name, state).is_none(),
            "one native row per selected table, never multiplied by full-text metadata"
        );
    }
    assert_eq!(tables.len(), 2, "both owned ordinary tables remain available");
    tables
}

async fn stream_tables(
    reader: &mut ReadSession,
    fixture: &Fixture<'_>,
    denied: bool,
) -> Result<Vec<(String, MssqlTableFullText)>> {
    // Available reads use the shipped streamed partition-statistics layout; denied metadata uses the existing catalog fallback.
    let layout = Layout::new(!denied);
    reader
        .begin_stream(
            &select_sql(&layout),
            vec![Parameter::Integer((MAX_APPLICATION_TABLES + 1) as i64)],
            MAX_APPLICATION_TABLES + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    let mut retained = Vec::new();
    let mut rows = 0usize;
    loop {
        let batch = reader.next_batch().await.map_err(ReadError::into_error)?;
        rows += batch.rows.len();
        assert!(
            rows <= MAX_APPLICATION_TABLES,
            "native overflow is a failed source, not a truncated successful prefix"
        );
        for row in batch.rows {
            let table = decode(&row, &layout)?;
            if !fixture.owns(&table.schema, &table.name) {
                continue;
            }
            fixture.assert_identity(&table);
            assert_rowstore_configuration(&table.storage_configuration);
            if denied {
                for name in [
                    "full_text_object_id",
                    "full_text_unique_index_id",
                    "full_text_catalog_id",
                    "full_text_enabled",
                    "full_text_change_tracking",
                    "full_text_last_crawl_completed",
                ] {
                    assert_eq!(
                        row.columns().find(|(column, _)| *column == name).map(|(_, value)| value),
                        Some(&Val::Null),
                        "{name}"
                    );
                }
            }
            let record = TelemetryRecord::Operational(OperationalObservation {
                source: crate::sources::TABLES.name.into(),
                key: table.identity.key()?,
                fact: OperationalFact::MssqlTable(Box::new(table.clone())),
            });
            assert_eq!(
                serde_json::from_slice::<TelemetryRecord>(&serde_json::to_vec(&record).unwrap()).unwrap(),
                record
            );
            retained.push((table.name, table.full_text));
        }
        if batch.complete {
            break;
        }
    }
    Ok(retained)
}

fn assert_rowstore_configuration(configuration: &MssqlTableStorageConfiguration) {
    assert_eq!(
        configuration,
        &MssqlTableStorageConfiguration {
            memory_optimized: Some(false),
            durability: Some(0),
            durability_desc: Some("SCHEMA_AND_DATA".into()),
        },
        "the owned ordinary tables retain their exact native catalog settings"
    );
}
