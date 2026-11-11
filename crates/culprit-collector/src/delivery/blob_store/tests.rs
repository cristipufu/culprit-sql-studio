use super::super::tests::{contract, prepared, problem};
use super::*;
use async_trait::async_trait;
use std::{collections::BTreeMap, sync::Mutex};

#[tokio::test]
async fn publication_retirement_is_fenced_atomic_and_recovers_a_lost_acknowledgement_without_erasing_history() {
    use crate::delivery::{file_sink::FileTelemetrySink, sink::*};
    let root = tempfile::tempdir().unwrap();
    let blobs = Arc::new(MemoryBlobs::default());
    let store = Arc::new(BlobRecoveryStore::create(blobs.clone(), contract()).await.unwrap());
    let stale = store.acquire().await.unwrap();
    let first = prepared(root.path(), 1);
    let range = stage_and_commit(&*store, &stale, None, &first).await.unwrap();
    let second = prepared(root.path(), 2);
    stage_and_commit(&*store, &stale, Some(&range.manifest().range_id), &second).await.unwrap();
    let before = store.pending_ranges(None, 1, RecoveryOrder::OldestFirst).await.unwrap();
    assert!(before.next_cursor.is_some());
    let sink = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store.clone()).unwrap();
    let attempt = DeliveryAttempt::new(1).unwrap();
    dispatch_range(&sink, &range, &attempt).await.unwrap();
    let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
    sink.publish(&verified, &attempt).await.unwrap();
    let publication = read_publication(&sink, &range).await.unwrap();
    let fence = store.acquire().await.unwrap();
    assert_eq!(problem(store.retire(&stale, &publication).await.err().unwrap()), DeliveryProblem::Fenced);
    blobs.fail(ROOT_KEY, false);
    assert_eq!(problem(store.retire(&fence, &publication).await.err().unwrap()), DeliveryProblem::Transport);
    assert_eq!(store.pending_ranges(None, 1, RecoveryOrder::OldestFirst).await.unwrap().ranges, before.ranges);
    blobs.fail(ROOT_KEY, true);
    assert_eq!(problem(store.retire(&fence, &publication).await.err().unwrap()), DeliveryProblem::Transport);
    let reopened = BlobRecoveryStore::open(blobs, contract()).await.unwrap();
    let pending = reopened.pending_ranges(None, 128, RecoveryOrder::OldestFirst).await.unwrap();
    assert_eq!(pending.ranges, [second.manifest().clone()]);
    assert!(pending.next_cursor.is_none());
    let continued =
        reopened.pending_ranges(before.next_cursor.as_deref(), 1, RecoveryOrder::OldestFirst).await.unwrap();
    assert_eq!(continued.ranges, [second.manifest().clone()]);
    assert!(continued.next_cursor.is_none());
    reopened.retire(&fence, &publication).await.unwrap();
    assert_eq!(reopened.recover(None, 128).await.unwrap().ranges.len(), 2);
    assert_eq!(reopened.load(&range.manifest().range_id).await.unwrap(), *range.manifest());
}

#[derive(Clone)]
struct Object {
    etag: String,
    bytes: Vec<u8>,
}
struct Fault {
    key: String,
    after_write: bool,
}
#[derive(Default)]
struct Memory {
    objects: BTreeMap<String, Object>,
    sequence: u64,
    fault: Option<Fault>,
    reads: BTreeMap<String, usize>,
}
#[derive(Default)]
struct MemoryBlobs(Mutex<Memory>, Mutex<Option<Arc<Pause>>>);
#[derive(Default)]
struct Pause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl MemoryBlobs {
    fn fail(&self, key: &str, after_write: bool) {
        self.0.lock().unwrap().fault = Some(Fault { key: key.into(), after_write });
    }
    fn write(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite> {
        let mut state = self.0.lock().unwrap();
        let allowed = match condition {
            WriteCondition::Absent => !state.objects.contains_key(key),
            WriteCondition::Matches(etag) => state.objects.get(key).is_some_and(|object| object.etag == etag),
        };
        if !allowed {
            return Ok(BlobWrite::ConditionNotMet);
        }
        let fault = if state.fault.as_ref().is_some_and(|fault| fault.key == key) { state.fault.take() } else { None };
        if fault.as_ref().is_some_and(|fault| !fault.after_write) {
            return Err(transport_error());
        }
        state.sequence += 1;
        let etag = format!("\"{}\"", state.sequence);
        state.objects.insert(key.into(), Object { etag, bytes });
        if fault.is_some() {
            return Err(transport_error());
        }
        Ok(BlobWrite::Written)
    }
}
#[async_trait]
impl BlobTransport for MemoryBlobs {
    async fn get(&self, key: &str) -> Result<Option<BlobRead>> {
        let mut state = self.0.lock().unwrap();
        *state.reads.entry(key.into()).or_default() += 1;
        Ok(state.objects.get(key).cloned().map(|object| BlobRead {
            etag: object.etag,
            bytes: object.bytes.len() as u64,
            body: Box::pin(std::io::Cursor::new(object.bytes)),
        }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite> {
        let pause = if key == ROOT_KEY { self.1.lock().unwrap().take() } else { None };
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        self.write(key, bytes, condition)
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<BlobWrite> {
        let file = tokio::fs::File::open(path).await.map_err(|_| transport_error())?;
        let mut contents = Vec::new();
        file.take(bytes + 1).read_to_end(&mut contents).await.map_err(|_| transport_error())?;
        if contents.len() as u64 != bytes {
            return Err(integrity());
        }
        self.write(key, contents, WriteCondition::Absent)
    }
}

#[tokio::test]
async fn committed_capabilities_are_reused_within_one_process_but_not_across_stores_or_restart() {
    use crate::delivery::committed_cache::CommittedRangeCache;
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared(root.path(), 1);
    let (blobs, store, fence) = fixture().await;
    let committed = stage_and_commit(&store, &fence, None, &prepared).await.unwrap();
    let id = &committed.manifest().range_id;
    let sha = committed.manifest_digest();
    let bytes = serde_json::to_vec(committed.manifest()).unwrap().len();
    let key = format!("manifests/{sha}.json");
    let reads = || blobs.0.lock().unwrap().reads.get(&key).copied().unwrap_or(0);
    let before = reads();
    let mut cache = CommittedRangeCache::default();
    for _ in 0..100 {
        assert_eq!(cache.get(&store, id, sha, bytes).await.unwrap().manifest(), committed.manifest());
    }
    assert_eq!(reads(), before + 1, "receipt pages must not download the immutable native manifest again");
    assert_eq!(
        problem(cache.get(&store, id, &digest(b"different"), bytes).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    assert_eq!(problem(cache.get(&store, id, sha, bytes + 1).await.err().unwrap()), DeliveryProblem::Integrity);
    assert_eq!(reads(), before + 1, "a conflicting local manifest cannot reuse a capability");
    let (_, empty_store, _) = fixture().await;
    assert!(cache.get(&empty_store, id, sha, bytes).await.is_err(), "capabilities are scoped to the recovery store");
    let mut restarted = CommittedRangeCache::default();
    restarted.get(&store, id, sha, bytes).await.unwrap();
    assert_eq!(reads(), before + 2, "a new process must ask the authoritative store again");
}
async fn fixture() -> (Arc<MemoryBlobs>, BlobRecoveryStore, Fence) {
    let blobs = Arc::new(MemoryBlobs::default());
    let store = BlobRecoveryStore::create(blobs.clone(), contract()).await.unwrap();
    let fence = store.acquire().await.unwrap();
    (blobs, store, fence)
}
#[tokio::test]
async fn activity_recovery_heads_follow_the_same_root_cas_even_when_index_writes_or_acknowledgements_fail() {
    let root = tempfile::tempdir().unwrap();
    let prepared = crate::delivery::batch_tests::quiet_activity(root.path());
    let (blobs, store, fence) = fixture().await;
    blobs.fail(ROOT_KEY, false);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &prepared).await), DeliveryProblem::Transport);
    assert!(store.activity_heads(None, 10).await.unwrap().heads.is_empty());
    blobs.fail(ROOT_KEY, true);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &prepared).await), DeliveryProblem::Transport);
    let heads = store.activity_heads(None, 10).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    assert_eq!(heads.high_sequence, 1);
    assert_eq!(heads.heads[0].range_id, prepared.manifest().range_id);
    assert_eq!(heads.heads[0].manifest_digest, prepared.manifest().digest().unwrap());
    drop(store);
    let reopened = BlobRecoveryStore::open(blobs, contract()).await.unwrap();
    assert_eq!(reopened.activity_heads(None, 10).await.unwrap().heads, heads.heads);
    assert_eq!(reopened.load(&heads.heads[0].range_id).await.unwrap(), *prepared.manifest());
    assert!(reopened.captured_range(&prepared.manifest().source.captured_range().unwrap()).await.unwrap().is_none());
}
fn failure<T>(result: Result<T>) -> DeliveryProblem {
    problem(result.err().expect("must fail"))
}

#[tokio::test]
async fn restores_exact_committed_payloads_and_checkpoints_after_local_capture_loss() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    let committed = stage_and_commit(&store, &fence, None, &first).await.unwrap();
    let expected = tokio::fs::read(first.batch_path(&first.manifest().batches[0])).await.unwrap();
    let second = prepared(temp.path(), 2);
    stage_and_commit(&store, &fence, Some(&first.manifest().range_id), &second).await.unwrap();
    let again = stage_and_commit(&store, &fence, None, &first).await.unwrap();
    assert_eq!(again.manifest_digest(), committed.manifest_digest());
    drop(first);
    drop(second);
    drop(temp);
    drop(store);
    let restored = BlobRecoveryStore::open(blobs, contract()).await.unwrap();
    assert_eq!(restored.checkpoint("db/resource-history").await.unwrap().as_deref(), Some("2"));
    let page = restored.recover(None, 1).await.unwrap();
    assert_eq!(page.high_sequence, 2);
    assert_eq!(page.ranges[0], *committed.manifest());
    let next = restored.recover(page.next_cursor.as_deref(), 1).await.unwrap();
    assert_eq!(next.ranges.len(), 1);
    assert!(next.next_cursor.is_none());
    let mut reader = restored.open_object(&PayloadObject::from(&committed.manifest().batches[0])).await.unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, expected);
}

#[tokio::test]
async fn stale_owner_wrong_stream_cursor_and_wrong_head_cannot_commit() {
    let temp = tempfile::TempDir::new().unwrap();
    let (_, store, stale) = fixture().await;
    let owner = store.acquire().await.unwrap();
    let first = prepared(temp.path(), 1);
    assert_eq!(failure(stage_and_commit(&store, &stale, None, &first).await), DeliveryProblem::Fenced);
    let second = prepared(temp.path(), 2);
    assert_eq!(failure(stage_and_commit(&store, &owner, None, &second).await), DeliveryProblem::Conflict);
    stage_and_commit(&store, &owner, None, &first).await.unwrap();
    assert_eq!(failure(stage_and_commit(&store, &stale, None, &first).await), DeliveryProblem::Fenced);
    assert_eq!(failure(stage_and_commit(&store, &owner, None, &second).await), DeliveryProblem::Conflict);
    assert_eq!(store.checkpoint("db/resource-history").await.unwrap().as_deref(), Some("1"));
}

#[tokio::test]
async fn orphan_manifest_is_not_a_committed_range() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    blobs.fail(ROOT_KEY, false);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Transport);
    assert_eq!(failure(store.load(&first.manifest().range_id).await), DeliveryProblem::NotReady);
    assert!(store.recover(None, 1).await.unwrap().ranges.is_empty());
    stage_and_commit(&store, &fence, None, &first).await.unwrap();
}

#[tokio::test]
async fn lost_commit_acknowledgement_and_missing_indexes_recover_without_resubmitting_sql() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    blobs.fail(ROOT_KEY, true);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Transport);
    assert_eq!(store.load(&first.manifest().range_id).await.unwrap(), *first.manifest());
    assert_eq!(store.recover(None, 1).await.unwrap().ranges, vec![first.manifest().clone()]);
    drop(store);
    let store = BlobRecoveryStore::open(blobs, contract()).await.unwrap();
    let next_owner = store.acquire().await.unwrap();
    let second = prepared(temp.path(), 2);
    stage_and_commit(&store, &next_owner, Some(&first.manifest().range_id), &second).await.unwrap();
    assert_eq!(store.recover(None, 2).await.unwrap().ranges.len(), 2);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Fenced);
}

#[tokio::test]
async fn interrupted_index_write_is_repaired_before_a_later_commit() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    blobs.fail("commits/00000000000000000001.json", false);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Transport);
    let second = prepared(temp.path(), 2);
    stage_and_commit(&store, &fence, Some(&first.manifest().range_id), &second).await.unwrap();
    assert_eq!(store.recover(None, 2).await.unwrap().ranges.len(), 2);
}

#[tokio::test]
async fn recovery_page_boundary_is_stable_while_new_ranges_arrive_and_cursor_tampering_is_refused() {
    let temp = tempfile::TempDir::new().unwrap();
    let (_, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    let second = prepared(temp.path(), 2);
    stage_and_commit(&store, &fence, None, &first).await.unwrap();
    stage_and_commit(&store, &fence, Some(&first.manifest().range_id), &second).await.unwrap();
    let page = store.recover(None, 1).await.unwrap();
    let third = prepared(temp.path(), 3);
    stage_and_commit(&store, &fence, Some(&second.manifest().range_id), &third).await.unwrap();
    let last = store.recover(page.next_cursor.as_deref(), 2).await.unwrap();
    assert_eq!(last.ranges, vec![second.manifest().clone()]);
    assert_eq!(last.high_sequence, 2);
    assert!(last.next_cursor.is_none());
    let mut cursor: RecoveryCursor = serde_json::from_str(page.next_cursor.as_ref().unwrap()).unwrap();
    cursor.head.manifest_digest = digest(b"orphan");
    assert_eq!(
        failure(store.recover(Some(&serde_json::to_string(&cursor).unwrap()), 1).await),
        DeliveryProblem::Integrity
    );
    cursor.head.manifest_digest = second.manifest().digest().unwrap();
    cursor.high_sequence = 1;
    assert_eq!(
        failure(store.recover(Some(&serde_json::to_string(&cursor).unwrap()), 1).await),
        DeliveryProblem::Integrity
    );
}

#[tokio::test]
async fn corrupt_payload_or_manifest_never_authorizes_recovery_or_checkpoint_advance() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    let batch = &first.manifest().batches[0];
    store.stage(&PayloadObject::from(batch), &first.batch_path(batch)).await.unwrap();
    blobs.0.lock().unwrap().objects.get_mut(&batch.object).unwrap().bytes[0] ^= 1;
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Integrity);
    assert!(store.checkpoint_head().await.unwrap().is_none());
    blobs.0.lock().unwrap().objects.remove(&batch.object);
    stage_and_commit(&store, &fence, None, &first).await.unwrap();
    let key = format!("manifests/{}.json", first.manifest().digest().unwrap());
    blobs.0.lock().unwrap().objects.get_mut(&key).unwrap().bytes[0] ^= 1;
    assert_eq!(failure(store.load(&first.manifest().range_id).await), DeliveryProblem::Integrity);
}

#[tokio::test]
async fn namespace_creation_and_contract_identity_are_explicit() {
    let blobs = Arc::new(MemoryBlobs::default());
    assert_eq!(failure(BlobRecoveryStore::open(blobs.clone(), contract()).await), DeliveryProblem::NotReady);
    BlobRecoveryStore::create(blobs.clone(), contract()).await.unwrap();
    assert_eq!(failure(BlobRecoveryStore::create(blobs.clone(), contract()).await), DeliveryProblem::Conflict);
    let other = Contract::new("other".into(), "shard-0".into(), "file:warehouse".into(), digest(b"schema")).unwrap();
    assert_eq!(failure(BlobRecoveryStore::open(blobs, other).await), DeliveryProblem::DestinationChanged);
}

#[tokio::test]
async fn a_late_commit_from_a_replaced_owner_loses_the_checkpoint_compare_and_swap() {
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, stale) = fixture().await;
    let first = prepared(temp.path(), 1);
    let pause = Arc::new(Pause::default());
    *blobs.1.lock().unwrap() = Some(pause.clone());
    let delayed = async { stage_and_commit(&store, &stale, None, &first).await };
    let replace_owner = async {
        pause.entered.notified().await;
        let current = store.acquire().await.unwrap();
        pause.release.notify_one();
        current
    };
    let (result, owner) = tokio::join!(delayed, replace_owner);
    assert_eq!(failure(result), DeliveryProblem::Conflict);
    assert!(store.checkpoint_head().await.unwrap().is_none());
    assert_eq!(failure(store.load(&first.manifest().range_id).await), DeliveryProblem::NotReady);
    stage_and_commit(&store, &owner, None, &first).await.unwrap();
}

#[tokio::test]
async fn exact_native_range_receipts_survive_interrupted_indexing_without_claiming_current_or_other_database_coverage()
{
    use culprit_core::telemetry::{CapturedRange, TelemetryRange};
    let temp = tempfile::TempDir::new().unwrap();
    let (blobs, store, fence) = fixture().await;
    let first = prepared(temp.path(), 1);
    let range = first.manifest().source.captured_range().unwrap();
    assert!(store.captured_range(&range).await.unwrap().is_none());
    let key = format!("coverage/{}.json", coverage_key(&range).unwrap().unwrap());
    blobs.fail(&key, false);
    assert_eq!(failure(stage_and_commit(&store, &fence, None, &first).await), DeliveryProblem::Transport);
    assert_eq!(store.captured_range(&range).await.unwrap().unwrap(), first.manifest().capture);
    let current = CapturedRange { scope_id: range.scope_id.clone(), range: TelemetryRange::Current };
    assert!(store.captured_range(&current).await.unwrap().is_none());
    let same = CapturedRange {
        scope_id: range.scope_id.clone(),
        range: TelemetryRange::TimeWindow {
            from: "2020-01-01T08:00:00+02:00".into(),
            until: "2020-01-01T08:01:00+02:00".into(),
        },
    };
    assert!(store.captured_range(&same).await.unwrap().is_some());
    let other = CapturedRange { scope_id: digest(b"other database"), range: range.range.clone() };
    assert!(store.captured_range(&other).await.unwrap().is_none());
    let second = prepared(temp.path(), 2);
    stage_and_commit(&store, &fence, Some(&first.manifest().range_id), &second).await.unwrap();
    let forged_key = format!("coverage/{}.json", coverage_key(&other).unwrap().unwrap());
    {
        let mut memory = blobs.0.lock().unwrap();
        let copied = memory.objects.get(&key).unwrap().clone();
        memory.objects.insert(forged_key, copied);
    }
    assert_eq!(failure(store.captured_range(&other).await), DeliveryProblem::Integrity);
}
