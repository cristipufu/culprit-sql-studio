//! The same executable recovery contract runs against every native object provider.
use super::{ObjectRecoveryStore, transport::*};
use crate::delivery::recovery::RecoveryStore;
use culprit_core::telemetry::DeliveryProblem;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
const DOWNLOAD_CHUNK_BYTES: usize = super::bridge::DOWNLOAD_CHUNK_BYTES;
fn typed(error: culprit_core::Error) -> DeliveryProblem {
    let Some(culprit_core::ErrorDetail::Delivery(problem)) = error.detail else { panic!("typed delivery failure") };
    problem
}
pub async fn verify_recovery_contract(transport: Arc<dyn ObjectTransport>) {
    let contract = fixture_contract();
    let store = ObjectRecoveryStore::create(transport.clone(), contract.clone()).await.unwrap();
    let old = store.acquire().await.unwrap();
    let owner = store.acquire().await.unwrap();
    assert_eq!(owner.epoch(), old.epoch() + 1);
    assert!(store.checkpoint_head().await.unwrap().is_none());
    let temp = tempfile::NamedTempFile::new().unwrap();
    let bytes = vec![b'x'; 2 * DOWNLOAD_CHUNK_BYTES + 7];
    std::fs::write(temp.path(), &bytes).unwrap();
    let hash = culprit_core::telemetry::digest(&bytes);
    let payload = crate::delivery::recovery::PayloadObject {
        object: format!("payloads/{hash}.ndjson"),
        sha256: hash,
        bytes: bytes.len() as u64,
    };
    store.stage(&payload, temp.path()).await.unwrap();
    store.stage(&payload, temp.path()).await.unwrap();
    let reopened = ObjectRecoveryStore::open(transport.clone(), contract).await.unwrap();
    assert_eq!(reopened.id(), store.id());
    let mut reader = reopened.open_object(&payload).await.unwrap();
    let mut restored = Vec::new();
    reader.read_to_end(&mut restored).await.unwrap();
    assert_eq!(restored, bytes);
    let root = transport.get("recovery.json").await.unwrap().unwrap();
    assert!(matches!(
        transport.put("recovery.json", vec![], WriteCondition::Matches("\"stale\"")).await.unwrap(),
        ObjectWrite::ConditionNotMet
    ));
    assert!(!root.etag.is_empty());

    let manifest = empty_range(fixture_contract(), owner.epoch());
    let empty = tempfile::NamedTempFile::new().unwrap();
    let mut staged = Vec::new();
    for batch in &manifest.batches {
        staged.push(store.stage(&crate::delivery::recovery::PayloadObject::from(batch), empty.path()).await.unwrap());
    }
    assert_eq!(typed(store.commit(&old, None, &manifest, &staged).await.err().unwrap()), DeliveryProblem::Fenced);
    store.commit(&owner, None, &manifest, &staged).await.unwrap();
    drop(empty);
    drop(temp);
    drop(store);
    assert_eq!(reopened.checkpoint("db/resource-history").await.unwrap().as_deref(), Some("1"));
    assert_eq!(reopened.load(&manifest.range_id).await.unwrap(), manifest);
    let page = reopened.recover(None, 1).await.unwrap();
    assert_eq!(page.ranges, vec![manifest]);
    assert_eq!(page.high_sequence, 1);
    assert!(page.next_cursor.is_none());
}

fn fixture_contract() -> crate::delivery::contract::Contract {
    crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "adx:test".into(),
        culprit_core::telemetry::digest(b"schema"),
    )
    .unwrap()
}

fn empty_range(contract: crate::delivery::contract::Contract, epoch: u64) -> crate::delivery::evidence::RangeManifest {
    use crate::delivery::evidence::*;
    use culprit_core::telemetry::*;
    let capture = CaptureIdentity {
        observation_id: "object-observation".into(),
        revision: Some(CaptureRevision {
            outbox_id: "object-outbox".into(),
            owner_epoch: epoch.to_string(),
            sequence: "1".into(),
        }),
    };
    let source = CollectionDescription {
        database: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        ),
        source: "resources".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
            observed_at: "2020-01-01T06:02:00Z".into(),
            available_from: Some("2020-01-01T05:00:00Z".into()),
            available_until: Some("2020-01-01T06:01:30Z".into()),
            expected_samples: 1,
            nominal_period_ms: 15000,
        }),
    };
    let batches = source
        .observation
        .collection()
        .streams()
        .iter()
        .map(|&stream| {
            let sha256 = digest(b"");
            BatchManifest {
                batch_id: batch_id(&capture.observation_id, stream).unwrap(),
                stream,
                object: format!("payloads/{sha256}.ndjson"),
                sha256,
                bytes: 0,
                rows: vec![],
                delivery_required: true,
            }
        })
        .collect();
    let range_id = digest(&serde_json::to_vec(&(&contract, &capture)).unwrap());
    RangeManifest {
        contract,
        range_id,
        capture,
        source,
        collected_from: "2020-01-01T06:02:00Z".into(),
        collected_until: "2020-01-01T06:02:01Z".into(),
        read_metrics: None,
        batches,
        advances: vec![StreamAdvance { stream_key: "db/resource-history".into(), previous: None, next: "1".into() }],
    }
}
