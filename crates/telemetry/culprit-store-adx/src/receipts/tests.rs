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
        let catalog = SchemaCatalog::new().unwrap();
        Self::from_recovery(Recovery::new("adx:test", catalog.digest())).await
    }
    async fn from_recovery(recovery: Recovery) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let recovery = Arc::new(recovery);
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
        dispatch_range(&dispatcher, &range, &DeliveryAttempt::new(1).unwrap()).await.unwrap();
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
async fn shared_resources_survive_recovery_dispatch_and_exact_readback_without_database_attribution() {
    use culprit_collector::analytical::test_support::shared_resource;
    use culprit_core::telemetry::SharedResourceKind;
    let catalog = SchemaCatalog::new().unwrap();
    for kind in
        [SharedResourceKind::Instance, SharedResourceKind::Pool, SharedResourceKind::Cluster, SharedResourceKind::Node]
    {
        let (source, record) = shared_resource(kind, "namespace-a");
        let fixture =
            Fixture::from_recovery(Recovery::capture("adx:test", catalog.digest(), source, vec![record], 2)).await;
        let schema = fixture.catalog.get(fixture.range.manifest().batches[0].stream).unwrap();
        let resource = schema.columns().iter().position(|column| column.name == "ResourceId").unwrap();
        let resource_kind = schema.columns().iter().position(|column| column.name == "ResourceKind").unwrap();
        assert_eq!(fixture.values[resource], fixture.range.manifest().source.target.telemetry_id().unwrap());
        assert_eq!(fixture.values[resource_kind], serde_json::to_value(kind).unwrap());
        let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone()])]);
        assert_eq!(
            reader.receipts(&fixture.range, None, 1000).await.unwrap().rows,
            fixture.range.manifest().batches[0].rows
        );
        let (other, _) = shared_resource(kind, "namespace-b");
        let mut changed = fixture.values.clone();
        changed[resource] = json!(other.target.telemetry_id().unwrap());
        let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![changed])]);
        assert!(reader.receipts(&fixture.range, None, 1000).await.is_err());
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
        ("ResourceKind", json!("pool")),
        ("ResourceId", json!("f".repeat(64))),
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

#[tokio::test]
async fn observed_wait_resources_survive_exact_readback_and_a_changed_native_target_is_rejected() {
    use culprit_collector::analytical::test_support::activity_with_wait_resource;
    use culprit_core::WaitResourceKind;
    let catalog = SchemaCatalog::new().unwrap();
    for kind in [WaitResourceKind::MemoryPage, WaitResourceKind::PageIo, WaitResourceKind::DataLock] {
        let fixture = Fixture::from_recovery(activity_with_wait_resource("adx:test", catalog.digest(), 2, kind)).await;
        let schema = fixture.catalog.get(fixture.range.manifest().batches[0].stream).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "WaitResources").unwrap();
        let resources: Value = serde_json::from_str(fixture.values[column].as_str().unwrap()).unwrap();
        assert_eq!(resources[0]["kind"], serde_json::to_value(kind).unwrap());
        assert_eq!(resources[0]["native"]["id"], "native-wait-resource");
        let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone()])]);
        let page = reader.receipts(&fixture.range, None, 1000).await.unwrap();
        assert_eq!(page.rows, fixture.range.manifest().batches[0].rows);
        let mut changed = resources;
        changed[0]["native"]["id"] = json!("another-resource");
        let mut values = fixture.values.clone();
        values[column] = json!(changed.to_string());
        let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![values])]);
        assert!(reader.receipts(&fixture.range, None, 1000).await.is_err());
    }
}

#[tokio::test]
async fn native_query_compilation_memory_survives_readback_and_changed_memory_cannot_verify() {
    let catalog = SchemaCatalog::new().unwrap();
    let recovery = culprit_collector::analytical::test_support::query_compilation("adx:test", catalog.digest());
    let fixture = Fixture::from_recovery(recovery).await;
    let schema = fixture.catalog.get(fixture.range.manifest().batches[0].stream).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut details: Value = serde_json::from_str(fixture.values[column].as_str().unwrap()).unwrap();
    assert_eq!(details["compilations"], "9007199254740993");
    assert_eq!(details["memory"]["average_kb"], 32768.5);
    assert_eq!(details["memory"]["last_kb"], 16384.0);
    assert_eq!(details["memory"]["maximum_kb"], 65536.0);
    let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![fixture.values.clone()])]);
    assert_eq!(
        reader.receipts(&fixture.range, None, 1000).await.unwrap().rows,
        fixture.range.manifest().batches[0].rows
    );
    details["memory"]["last_kb"] = json!(1.0);
    let mut changed = fixture.values.clone();
    changed[column] = json!(details.to_string());
    let (reader, _) = fixture.reader(vec![Reply::Audit(0), Reply::Rows(vec![changed])]);
    assert!(reader.receipts(&fixture.range, None, 1000).await.is_err());
}
