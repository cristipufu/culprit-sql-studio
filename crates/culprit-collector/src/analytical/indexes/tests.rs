use super::*;
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Memory {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    reads: AtomicUsize,
}
#[async_trait]
impl BlobTransport for Memory {
    async fn get(&self, key: &str) -> Result<Option<BlobRead>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.objects.lock().unwrap().get(key).cloned().map(|bytes| BlobRead {
            etag: "fixture".into(),
            bytes: bytes.len() as u64,
            body: Box::pin(std::io::Cursor::new(bytes)),
        }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite> {
        assert!(matches!(condition, WriteCondition::Absent));
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(key) {
            return Ok(BlobWrite::ConditionNotMet);
        }
        objects.insert(key.into(), bytes);
        Ok(BlobWrite::Written)
    }
    async fn put_file(&self, _: &str, _: &std::path::Path, _: u64) -> Result<BlobWrite> {
        unreachable!("projection indexes never spool source payload files")
    }
}
fn expected(byte: u8) -> ExpectedRow {
    ExpectedRow::new([byte; 32], 1024).unwrap()
}
fn key(schema: u8, rows: usize) -> IndexKey {
    let hash = |byte| culprit_core::telemetry::digest(&[byte]);
    IndexKey::new(&hash(1), &hash(2), &hash(3), &hash(4), &hash(schema), rows).unwrap()
}
#[tokio::test]
async fn one_bounded_index_is_cached_for_all_receipt_pages_and_replacement_drops_the_old_cache() {
    let memory = Arc::new(Memory::default());
    let indexes = ProjectionIndexes::new(memory.clone());
    let first = key(5, 2000);
    let values = vec![expected(7); 2000];
    indexes.put(&first, &values).await.unwrap();
    for _ in 0..3 {
        assert_eq!(&*indexes.read(&first).await.unwrap(), values.as_slice());
    }
    assert_eq!(memory.reads.load(Ordering::Relaxed), 1);
    let second = key(6, 1);
    indexes.put(&second, &[expected(9)]).await.unwrap();
    indexes.read(&second).await.unwrap();
    indexes.read(&first).await.unwrap();
    assert_eq!(memory.reads.load(Ordering::Relaxed), 3);
}
#[tokio::test]
async fn duplicate_writes_require_identical_derivation_and_corruption_or_wrong_identity_is_rejected() {
    let memory = Arc::new(Memory::default());
    let indexes = ProjectionIndexes::new(memory.clone());
    let first = key(5, 1);
    indexes.put(&first, &[expected(7)]).await.unwrap();
    indexes.put(&first, &[expected(7)]).await.unwrap();
    assert!(indexes.put(&first, &[expected(8)]).await.is_err());
    memory.objects.lock().unwrap().get_mut(&first.object).unwrap()[HEADER_BYTES] ^= 1;
    assert!(indexes.read(&first).await.is_err());
    let good = encode(&first, &[expected(7)]);
    let other = key(6, 1);
    memory.objects.lock().unwrap().insert(other.object.clone(), good);
    assert!(indexes.read(&other).await.is_err());
}
#[tokio::test]
async fn empty_batches_need_no_object_and_oversized_or_inconsistent_indexes_are_refused() {
    let memory = Arc::new(Memory::default());
    let indexes = ProjectionIndexes::new(memory.clone());
    let empty = key(5, 0);
    indexes.put(&empty, &[]).await.unwrap();
    assert!(indexes.read(&empty).await.unwrap().is_empty());
    assert!(memory.objects.lock().unwrap().is_empty());
    assert_eq!(memory.reads.load(Ordering::Relaxed), 0);
    let first = key(5, 1);
    assert!(indexes.put(&first, &[]).await.is_err());
    let maximum = key(5, MAX_RANGE_RECORDS);
    assert_eq!(maximum.bytes(), 3_600_072);
    let hash = culprit_core::telemetry::digest(b"id");
    assert!(IndexKey::new(&hash, &hash, &hash, &hash, &hash, MAX_RANGE_RECORDS + 1).is_err());
}
