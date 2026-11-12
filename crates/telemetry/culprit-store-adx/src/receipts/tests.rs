use super::*;
use crate::sink::AdxStore;
use crate::{
    projection::ScalarType,
    test_support::{Blobs, Queries, Recovery, Reply},
};
use async_trait::async_trait;
use culprit_collector::analytical::{indexes::ProjectionIndexes, receipts::ReceiptReader};
use culprit_collector::delivery::{recovery::reopen, sink::*};
use std::sync::Mutex;

struct Fixture {
    _temp: tempfile::TempDir,
    range: CommittedRange,
    catalog: Arc<SchemaCatalog>,
    indexes: Arc<ProjectionIndexes>,
    values: Vec<Value>,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let blobs = Arc::new(Blobs::default());
        let indexes = Arc::new(ProjectionIndexes::new(blobs.clone()));
        let dispatcher = crate::test_support::event_grid_sink(
            range.manifest().contract.clone(),
            recovery,
            blobs.clone(),
            Arc::new(Queries::default()),
            temp.path(),
        )
        .unwrap();
        dispatcher
            .dispatch(&range, &DeliveryAttempt::new(1).unwrap(), &range.manifest().batches[0].batch_id)
            .await
            .unwrap();
        let objects = blobs.objects.lock().unwrap();
        let wire = objects.iter().find(|(key, _)| key.starts_with("ready/")).unwrap().1;
        let row: Value = serde_json::from_slice(wire).unwrap();
        let schema = catalog.get(range.manifest().batches[0].stream).unwrap();
        let values = schema
            .columns()
            .iter()
            .map(|column| {
                let value = &row[&column.name];
                match column.kind {
                    ScalarType::JsonContainer => value.as_str().map_or_else(|| json!(""), |text| json!(text)),
                    ScalarType::ExactCounter => {
                        json!(value.as_str().map(|text| format!("{text}.0")).unwrap_or_default())
                    }
                    _ => value.clone(),
                }
            })
            .collect();
        drop(objects);
        Self { _temp: temp, range, catalog, indexes, values }
    }
    fn reader(&self, replies: Vec<Reply>) -> (ReceiptReader, Arc<Queries>) {
        let query = Arc::new(Queries { replies: Mutex::new(replies.into()), requests: Mutex::new(vec![]) });
        (
            ReceiptReader::new(
                Arc::new(
                    AdxStore::new(
                        query.clone(),
                        Arc::new(crate::ready::ReadyDispatcher::new(Arc::new(Blobs::default()))),
                    )
                    .unwrap(),
                ),
                self.catalog.clone(),
                self.indexes.clone(),
            ),
            query,
        )
    }
}
#[tokio::test]
async fn readback_compares_every_projected_field_and_only_then_releases_the_native_receipt() {
    let fixture = Fixture::new().await;
    let (reader, queries) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone()])]);
    let page = reader.receipts(&fixture.range, None, 1000).await.unwrap();
    assert_eq!(page.rows, fixture.range.manifest().batches[0].rows);
    assert!(page.complete);
    assert!(page.next_cursor.is_none());
    let queries = queries.requests.lock().unwrap();
    assert_eq!(queries.len(), 2);
    assert!(queries[0].text.contains("CulpritDeadlocksV1"));
    assert!(queries[0].text.contains("long(-1)"));
    assert_eq!(queries[1].parameters["pAfter"], 0);
    assert_eq!(queries[1].parameters["pUntil"], 1);
    assert!(queries[1].text.contains("Metrics=tostring(Metrics)"));
    assert!(queries[1].text.contains("| distinct ContractId, ManifestSha256"));
    for column in &queries[1].columns {
        assert!(queries[1].text.contains(&column.column_name));
    }
}
#[tokio::test]
async fn count_matches_and_opaque_native_hashes_cannot_hide_wrong_typed_values_or_identity() {
    let fixture = Fixture::new().await;
    let schema = fixture.catalog.get(fixture.range.manifest().batches[0].stream).unwrap();
    for (name, value) in [
        ("Metrics", json!(r#"[{"value":99}]"#)),
        ("RecordId", json!("f".repeat(64))),
        ("CollectedFrom", json!("2020-01-01T00:00:00Z")),
        ("HasDataset", json!(true)),
        ("RowOrdinal", json!(0)),
    ] {
        let mut values = fixture.values.clone();
        let column = schema.columns().iter().position(|column| column.name == name).unwrap();
        values[column] = value;
        let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![values])]);
        assert!(reader.receipts(&fixture.range, None, 1000).await.is_err(), "{name}");
    }
    let (reader, _) =
        fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone(), fixture.values.clone()])]);
    assert!(reader.receipts(&fixture.range, None, 1000).await.is_err());
}
struct Sink(ReceiptReader);
#[async_trait]
impl TelemetrySink for Sink {
    async fn validate_contract(&self, _: &culprit_collector::delivery::contract::Contract) -> Result<()> {
        Ok(())
    }
    async fn dispatch(&self, _: &CommittedRange, _: &DeliveryAttempt, _: &str) -> Result<Submission> {
        unreachable!()
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        self.0.receipts(range, cursor, page_rows).await
    }
    async fn publish(&self, _: &VerifiedRange, _: &DeliveryAttempt) -> Result<Publication> {
        unreachable!()
    }
    async fn publication_visible(&self, _: &Publication, _: &CommittedRange) -> Result<bool> {
        unreachable!()
    }
    async fn poll_updates(&self, _: Option<&str>, _: u32) -> Result<DeliveryUpdates> {
        unreachable!()
    }
}
#[tokio::test]
async fn missing_rows_unexpected_membership_and_native_query_failure_never_authorize_publication() {
    let fixture = Fixture::new().await;
    let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![])]);
    assert!(verify(&Sink(reader), &fixture.range, VerificationBudget::default()).await.is_err());
    for replies in [vec![Reply::Audit(1)], vec![Reply::Failure], vec![Reply::Audit(0), Reply::Failure]] {
        let (reader, _) = fixture.reader(replies);
        assert!(reader.receipts(&fixture.range, None, 1000).await.is_err());
    }
    let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone()])]);
    assert!(verify(&Sink(reader), &fixture.range, VerificationBudget::default()).await.is_ok());
}
#[tokio::test]
async fn stale_or_out_of_bounds_receipt_cursors_are_refused_before_a_destination_query() {
    let fixture = Fixture::new().await;
    let (reader, queries) = fixture.reader(vec![]);
    for cursor in [
        json!({"manifest":"wrong","batch":0,"after":0}),
        json!({"manifest":fixture.range.manifest_digest(),"batch":99,"after":0}),
        json!({"manifest":fixture.range.manifest_digest(),"batch":0,"after":99}),
    ] {
        assert!(reader.receipts(&fixture.range, Some(&cursor.to_string()), 1000).await.is_err());
    }
    assert!(queries.requests.lock().unwrap().is_empty());
}
