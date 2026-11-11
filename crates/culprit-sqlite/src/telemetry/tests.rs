use super::*;
use culprit_core::{ConnectionSpec, Provider};
use rusqlite::Connection;
struct Fixture(PathBuf);
impl Fixture {
    fn new(journal: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path =
            std::env::temp_dir().join(format!("culprit-export-{}-{nonce}-{sequence}.sqlite", std::process::id()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!("PRAGMA journal_mode={journal}; CREATE TABLE items(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT CHECK(length(name)>0)); CREATE INDEX ix_name ON items(name); INSERT INTO items(name) VALUES ('private-value'); ANALYZE;")).unwrap();
        Self(path)
    }
    fn provider(&self) -> crate::SqliteProvider {
        crate::SqliteProvider::new(&ConnectionSpec {
            database: Some(self.0.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-shm"));
    }
}
#[tokio::test]
async fn file_snapshots_close_before_paging_and_keep_native_values_private() {
    for mode in ["DELETE", "WAL"] {
        let fixture = Fixture::new(mode);
        let provider = fixture.provider();
        for dataset in [
            SnapshotDataset::Settings,
            SnapshotDataset::Operational,
            SnapshotDataset::Tables,
            SnapshotDataset::Indexes,
            SnapshotDataset::Columns,
            SnapshotDataset::ForeignKeys,
            SnapshotDataset::Objects,
            SnapshotDataset::Triggers,
            SnapshotDataset::Statistics,
            SnapshotDataset::Sequences,
        ] {
            let request = CollectionRequest {
                selection: CollectionSelection::Snapshot { dataset },
                budget: CollectionBudget { page_rows: 1, ..Default::default() },
            };
            let mut source = provider.telemetry_source(&request).unwrap();
            assert_eq!(source.query_count(), 0);
            let initial = source.describe().await.unwrap();
            let metrics = source.read_metrics().unwrap();
            assert_eq!(metrics.active_tasks, 0);
            assert_eq!(
                metrics.templates.iter().map(|template| template.completed_tasks).sum::<u32>(),
                source.query_count()
            );
            let templates = serde_json::to_string(&metrics).unwrap();
            for statement in ["PRAGMA query_only", "PRAGMA trusted_schema", "BEGIN DEFERRED", "ROLLBACK"] {
                assert!(templates.contains(statement), "{statement}: {templates}");
            }
            // A reader retained while destination paging stalls must not hold up a rollback-journal commit.
            let writer = Connection::open(&fixture.0).unwrap();
            writer.busy_timeout(Duration::from_millis(20)).unwrap();
            writer.execute("UPDATE items SET name='private-value'", []).unwrap();
            let mut cursor = None;
            loop {
                let CollectionRead::Page { records, next_cursor } = source
                    .read_page(dataset.stream(), cursor.as_deref())
                    .await
                    .unwrap_or_else(|error| panic!("{dataset:?}: {error:?}"))
                else {
                    panic!()
                };
                assert!(records.len() <= request.budget.page_rows as usize);
                assert!(records.iter().all(|record| record.stream() == dataset.stream()));
                assert!(records.iter().all(TelemetryRecord::is_valid), "{dataset:?}: {records:?}");
                for record in &records {
                    ExportedRecord::new(
                        &initial,
                        &CaptureIdentity { observation_id: "file-fixture".into(), revision: None },
                        record.clone(),
                    )
                    .unwrap();
                }
                assert!(!serde_json::to_string(&records).unwrap().contains("private-value"));
                cursor = next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
            assert!(completed.observation.covers_request(), "{dataset:?}: {:?}", completed.observation);
            assert_eq!(source.close().await, SourceShutdown::Quiescent);
        }
    }
}
#[tokio::test]
async fn file_replacement_invalidates_the_observation() {
    let fixture = Fixture::new("DELETE");
    let provider = fixture.provider();
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: Default::default(),
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    let initial = source.describe().await.unwrap();
    let old = fixture.0.with_extension("old");
    std::fs::rename(&fixture.0, &old).unwrap();
    drop(Connection::open(&fixture.0).unwrap());
    source.read_page(SnapshotDataset::Settings.stream(), None).await.unwrap();
    assert!(initial.completed(source.finish().await.unwrap()).unwrap().is_none());
    std::fs::remove_file(old).unwrap();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn query_limit_preserves_failed_read_accounting_and_still_rolls_back_and_closes() {
    let fixture = Fixture::new("DELETE");
    let provider = fixture.provider();
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget { max_queries: 4, ..Default::default() },
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    assert_eq!(source.describe().await.unwrap_err().detail, CollectionStop::QueryLimit.error().detail);
    assert_eq!(source.query_count(), 5, "four admitted statements plus mandatory rollback");
    let metrics = source.read_metrics().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert!(serde_json::to_string(&metrics).unwrap().contains("ROLLBACK"));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    Connection::open(&fixture.0).unwrap().execute("UPDATE items SET name='writer-after-failure'", []).unwrap();
}
