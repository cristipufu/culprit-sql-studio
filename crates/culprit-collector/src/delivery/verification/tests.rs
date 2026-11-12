use super::*;
use crate::delivery::{
    contract::Contract,
    driver::{DeliveryDriver, DeliveryPolicy},
    file_sink::FileTelemetrySink,
    file_store::FileRecoveryStore,
    journal::DeliveryJournal,
    recovery::*,
    sink::*,
    tests::{contract, prepared, problem},
};
use async_trait::async_trait;
use culprit_core::telemetry::DeliveryPhase;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct PagedSink<'a> {
    inner: &'a FileTelemetrySink,
    calls: Mutex<Vec<usize>>,
    stall_once: AtomicBool,
    dispatches: AtomicUsize,
    corrupt_final: bool,
}
#[async_trait]
impl TelemetrySink for PagedSink<'_> {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        self.inner.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.inner.dispatch(range, attempt, batch_id).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, _: u32) -> Result<ReceiptPage> {
        let page = cursor.unwrap_or("0").parse::<usize>().unwrap();
        self.calls.lock().unwrap().push(page);
        let stalled = page == 7 && self.stall_once.swap(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(if stalled { 300 } else { 20 })).await;
        let mut receipt = range.manifest().expected_receipts().next().unwrap().clone();
        if page == 19 && self.corrupt_final {
            receipt.critical_fields_valid = false;
        }
        Ok(ReceiptPage {
            rows: vec![receipt],
            next_cursor: (page < 19).then(|| (page + 1).to_string()),
            complete: true,
        })
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.inner.publish(range, attempt).await
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.inner.publication_visible(publication, range).await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        self.inner.poll_updates(cursor, limit).await
    }
}

#[tokio::test]
async fn verification_exceeding_one_request_deadline_resumes_across_restart_and_page_timeout_without_resubmitting() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared(root.path(), 1);
    let id = prepared.manifest().range_id.clone();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let warehouse = FileTelemetrySink::create(
        &root.path().join("warehouse"),
        contract(),
        std::sync::Arc::new(FileRecoveryStore::open(&root.path().join("recovery"), contract()).unwrap()),
    )
    .unwrap();
    let sink = PagedSink {
        inner: &warehouse,
        calls: Mutex::new(vec![]),
        stall_once: AtomicBool::new(true),
        dispatches: AtomicUsize::new(0),
        corrupt_final: false,
    };
    let path = root.path().join("delivery");
    let mut journal = DeliveryJournal::create(&path, contract()).unwrap();
    journal.register(&prepared).await.unwrap();
    let policy = DeliveryPolicy {
        visibility_delay_ms: 1,
        retry_delay_ms: 1,
        request_timeout_ms: 100,
        verification: VerificationBudget { page_rows: 1, max_pages: 64, max_physical_rows: 100 },
        ..Default::default()
    };
    let mut now = 0;
    {
        let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
        let mut driver = DeliveryDriver::new(&mut journal, &store, &sink, &mut capabilities, policy).unwrap();
        for _ in 0..32 {
            let result = driver.step(&id, owner.fence(), now).await;
            now += 1000;
            if let Err(error) = result {
                // A loaded host can also exceed this deliberately short deadline during setup or an early page.
                assert_eq!(problem(error), DeliveryProblem::Transport);
                continue;
            }
            if sink.calls.lock().unwrap().last() == Some(&4) {
                break;
            }
        }
    }
    let pending = journal.load(&id).await.unwrap();
    assert_eq!(pending.state.phase, DeliveryPhase::AwaitingData);
    assert_eq!(pending.state.verification.as_ref().unwrap().cursor.as_deref(), Some("5"));
    // Setup explicitly permits ambiguous request timeouts on a loaded host. Those may retry dispatch before
    // verification starts; resume and page timeouts must never add another submission after this checkpoint.
    let setup_dispatches = sink.dispatches.load(Ordering::SeqCst);
    assert!(setup_dispatches >= 1);
    let first_page_reads = sink.calls.lock().unwrap().iter().filter(|page| **page == 0).count();
    drop(pending);
    drop(journal);
    let mut journal = DeliveryJournal::open(&path, contract()).unwrap();
    let mut timeouts = 0;
    let mut retired = false;
    let mut first_pass = false;
    {
        let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
        let mut driver = DeliveryDriver::new(&mut journal, &store, &sink, &mut capabilities, policy).unwrap();
        for _ in 0..80 {
            now += 1000;
            match driver.step(&id, owner.fence(), now).await {
                Err(error) => {
                    assert_eq!(problem(error), DeliveryProblem::Transport);
                    timeouts += 1;
                }
                Ok(state) => {
                    if state.phase == DeliveryPhase::PublicationPending && !first_pass {
                        let calls = sink.calls.lock().unwrap();
                        assert_eq!(calls.iter().filter(|page| **page == 0).count(), first_page_reads);
                        assert!(calls.iter().filter(|page| **page == 7).count() >= 2);
                        assert_eq!(calls.last(), Some(&19));
                        first_pass = true;
                    }
                    if state.phase == DeliveryPhase::Retired {
                        retired = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(first_pass && retired);
    assert!(timeouts >= 1, "the stalled page must time out; an overloaded test host may also miss other deadlines");
    assert_eq!(sink.dispatches.load(Ordering::SeqCst), setup_dispatches);
    assert!(journal.load(&id).await.unwrap().state.verification.is_none());
}

#[tokio::test]
async fn a_late_invalid_page_cannot_finish_a_proof_or_persist_partial_page_progress() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared(root.path(), 1);
    let store = std::sync::Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let range = stage_and_commit(store.as_ref(), owner.fence(), None, &prepared).await.unwrap();
    let warehouse = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store).unwrap();
    let sink = PagedSink {
        inner: &warehouse,
        calls: Mutex::new(vec![]),
        stall_once: AtomicBool::new(false),
        dispatches: AtomicUsize::new(0),
        corrupt_final: true,
    };
    let mut progress =
        VerificationProgress::new(&range, VerificationBudget { page_rows: 1, max_pages: 64, max_physical_rows: 100 })
            .unwrap();
    for _ in 0..19 {
        assert!(matches!(progress.step(&sink, &range).await.unwrap(), PageVerification::Pending));
    }
    assert_eq!(progress.seen_rows, 1); // Membership is already present, but the remaining pages still matter.
    let before = serde_json::to_value(&progress).unwrap();
    assert_eq!(problem(progress.step(&sink, &range).await.err().unwrap()), DeliveryProblem::Integrity);
    assert_eq!(serde_json::to_value(&progress).unwrap(), before);
}
