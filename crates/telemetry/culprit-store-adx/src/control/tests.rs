use super::*;
use crate::{
    projection::ScalarType,
    test_support::{Blobs, Queries, Recovery, Reply},
};
use culprit_collector::delivery::sink::Publication;
use culprit_collector::delivery::{
    recovery::reopen,
    sink::{TelemetrySink, VerificationBudget, verify},
};
use culprit_core::telemetry::digest;
use std::{collections::VecDeque, sync::Mutex};

fn returned(schema: &super::super::schema::Schema, row: &Value) -> Vec<Value> {
    schema
        .columns()
        .iter()
        .map(|column| {
            let value = &row[&column.name];
            match column.kind {
                ScalarType::JsonContainer => value.as_str().map_or_else(|| json!(""), |text| json!(text)),
                ScalarType::ExactCounter => json!(value.as_str().unwrap_or_default()),
                _ => value.clone(),
            }
        })
        .collect()
}
#[tokio::test]
async fn data_readback_precedes_completion_and_publication_waits_for_exact_control_visibility() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = SchemaCatalog::new().unwrap();
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let blobs = Arc::new(Blobs::default());
    let query = Arc::new(Queries { replies: Mutex::new(VecDeque::new()), requests: Mutex::new(vec![]) });
    let sink = crate::sink::new(range.manifest().contract.clone(), recovery, blobs.clone(), query.clone(), temp.path())
        .unwrap();
    let attempt = DeliveryAttempt::new(1).unwrap();
    culprit_collector::delivery::sink::dispatch_range(&sink, &range, &attempt).await.unwrap();
    let values = {
        let objects = blobs.objects.lock().unwrap();
        assert!(!objects.keys().any(|key| key.starts_with("ready/CulpritPublishedRangesV1/")));
        let row: Value =
            serde_json::from_slice(objects.iter().find(|(key, _)| key.starts_with("ready/")).unwrap().1).unwrap();
        returned(catalog.get(range.manifest().batches[0].stream).unwrap(), &row)
    };
    query.replies.lock().unwrap().extend([Reply::Audit(0), Reply::Rows(vec![values])]);
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    let publication = sink.publish(&verified, &attempt).await.unwrap();
    // Repeating the same physical attempt requires identical immutable completion bytes.
    assert_eq!(sink.publish(&verified, &attempt).await.unwrap().manifest_digest, publication.manifest_digest);
    let values = {
        let objects = blobs.objects.lock().unwrap();
        let rows =
            objects.iter().filter(|(key, _)| key.starts_with("ready/CulpritPublishedRangesV1/")).collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        let row: Value = serde_json::from_slice(rows[0].1).unwrap();
        assert_eq!(row["ReceiptsSha256"], verified.receipts_digest());
        assert_eq!(row["RecoveryStoreId"], range.store_id());
        assert_eq!(serde_json::from_str::<Value>(row["Batches"].as_str().unwrap()).unwrap()[0]["rows"], 1);
        assert_eq!(
            serde_json::from_str::<Value>(row["SourceDescription"].as_str().unwrap()).unwrap()["continuity"],
            "unverified"
        );
        assert!(row["ReadMetrics"].is_null());
        returned(catalog.publication(), &row)
    };
    query.replies.lock().unwrap().push_back(Reply::Rows(vec![]));
    assert!(!sink.publication_visible(&publication, &range).await.unwrap());
    query.replies.lock().unwrap().push_back(Reply::Rows(vec![values.clone()]));
    assert!(sink.publication_visible(&publication, &range).await.unwrap());
    let mut wrong = values.clone();
    let digest_column =
        catalog.publication().columns().iter().position(|column| column.name == "ReceiptsSha256").unwrap();
    wrong[digest_column] = json!("a".repeat(64));
    query.replies.lock().unwrap().push_back(Reply::Rows(vec![wrong]));
    assert!(sink.publication_visible(&publication, &range).await.is_err());
    query.replies.lock().unwrap().push_back(Reply::Rows(vec![values.clone(), values]));
    assert!(sink.publication_visible(&publication, &range).await.is_err());
    query.replies.lock().unwrap().push_back(Reply::Failure);
    assert!(sink.publication_visible(&publication, &range).await.is_err());
    let forged = Publication { range_id: publication.range_id, manifest_digest: "a".repeat(64) };
    assert!(sink.publication_visible(&forged, &range).await.is_err());
}
#[tokio::test]
async fn an_empty_successful_capture_has_explicit_coverage_and_no_empty_data_blob() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = SchemaCatalog::new().unwrap();
    let mut recovery = Recovery::new("adx:test", catalog.digest());
    // The source description still declares the requested native sample interval and observed retention.
    // A successful empty batch must remain different from an unavailable source or a failed read.
    recovery.make_empty();
    let recovery = Arc::new(recovery);
    let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
    let blobs = Arc::new(Blobs::default());
    let query =
        Arc::new(Queries { replies: Mutex::new(VecDeque::from([Reply::Audit(0)])), requests: Mutex::new(vec![]) });
    let sink = crate::sink::new(range.manifest().contract.clone(), recovery, blobs.clone(), query.clone(), temp.path())
        .unwrap();
    let attempt = DeliveryAttempt::new(1).unwrap();
    culprit_collector::delivery::sink::dispatch_range(&sink, &range, &attempt).await.unwrap();
    assert!(blobs.objects.lock().unwrap().is_empty());
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    let publication = sink.publish(&verified, &attempt).await.unwrap();
    let values = {
        let objects = blobs.objects.lock().unwrap();
        assert_eq!(objects.len(), 1);
        let row: Value = serde_json::from_slice(objects.values().next().unwrap()).unwrap();
        let batches: Value = serde_json::from_str(row["Batches"].as_str().unwrap()).unwrap();
        assert_eq!(batches[0]["rows"], 0);
        assert_eq!(batches[0]["native_sha256"], digest(b""));
        returned(catalog.publication(), &row)
    };
    query.replies.lock().unwrap().push_back(Reply::Rows(vec![values]));
    assert!(sink.publication_visible(&publication, &range).await.unwrap());
}
#[test]
fn destination_and_schema_identity_are_checked_before_opening_scratch_or_dispatching_anything() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = SchemaCatalog::new().unwrap();
    let recovery = Arc::new(Recovery::new("adx:test", catalog.digest()));
    let blobs = Arc::new(Blobs::default());
    let query = Arc::new(Queries { replies: Mutex::new(VecDeque::new()), requests: Mutex::new(vec![]) });
    let contract = culprit_collector::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "adx:other".into(),
        catalog.digest().into(),
    )
    .unwrap();
    let scratch = temp.path().join("not-created");
    assert!(crate::sink::new(contract, recovery, blobs.clone(), query.clone(), &scratch).is_err());
    assert!(!scratch.exists());
    assert!(blobs.objects.lock().unwrap().is_empty());
    assert!(query.requests.lock().unwrap().is_empty());
}
