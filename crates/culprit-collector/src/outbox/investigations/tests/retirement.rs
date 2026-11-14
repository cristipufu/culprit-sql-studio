use super::*;
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{capture_reference, load_diagnostic_checkpoint, load_diagnostic_input},
    file_store::{FileOwnership, FileRecoveryStore},
};
use crate::outbox::Outbox;
use rusqlite::OptionalExtension;

struct Fixture {
    root: tempfile::TempDir,
    files: Files,
    store: FileRecoveryStore,
    owner: FileOwnership,
    contract: Contract,
    head: Option<String>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let contract = Contract::new("pipeline".into(), "shard".into(), "file:test".into(), digest(b"schema")).unwrap();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut files = Files::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
        files.recover().unwrap();
        files.begin_remote_epoch(owner.fence(), 0).unwrap();
        Self { root, files, store, owner, contract, head: None }
    }
    async fn archive(&mut self, capture: &CaptureIdentity, acknowledge: bool) -> CommittedRange {
        let prepared = self.files.prepare_delivery(&capture.observation_id, self.contract.clone(), vec![]).unwrap();
        let committed =
            stage_and_commit(&self.store, self.owner.fence(), self.head.as_deref(), &prepared).await.unwrap();
        self.head = Some(committed.manifest().range_id.clone());
        if acknowledge {
            self.files.mark_archived(&committed).unwrap();
        }
        committed
    }
    async fn native(&mut self, rows: Vec<ActivityObservation>, second: u32) -> (DiagnosticInput, CommittedRange) {
        let capture = baseline(&mut self.files, rows, true, second);
        let input = self.files.diagnostic_input(&capture.observation_id, 1).unwrap();
        let committed = self.archive(&capture, true).await;
        (input, committed)
    }
    async fn derive(&mut self, input: DiagnosticInput, update: DiagnosticUpdate, acknowledge: bool) -> CommittedRange {
        let capture = self.files.commit_diagnostics(input, update).unwrap();
        self.archive(&capture, acknowledge).await
    }
    async fn restarted(self) -> Self {
        let Self { root, files, store, owner, contract, head } = self;
        drop(files);
        drop(owner);
        let owner = store.acquire().await.unwrap();
        let high = store.recover(None, 1).await.unwrap().high_sequence;
        let mut files = Files::open(&root.path().join("outbox")).unwrap();
        files.recover().unwrap();
        files.begin_remote_epoch(owner.fence(), high).unwrap();
        Self { root, files, store, owner, contract, head }
    }
    fn count(&self, table: &str) -> u32 {
        let sql = match table {
            "inputs" => "SELECT count(*) FROM diagnostic_inputs",
            "steps" => "SELECT count(*) FROM diagnostic_steps",
            _ => panic!("known diagnostic metadata table"),
        };
        self.files.journal.conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }
    fn metadata_charge(&self, id: &str) -> Option<u64> {
        self.files
            .journal
            .conn
            .query_row("SELECT charged_bytes FROM diagnostic_inputs WHERE observation_id=?1", [id], |row| {
                row.get::<_, i64>(0).map(|bytes| u64::try_from(bytes).expect("nonnegative metadata charge"))
            })
            .optional()
            .unwrap()
    }
    async fn cache(&mut self, committed: &CommittedRange) {
        let recovered = load_diagnostic_input(&self.store, &committed.manifest().range_id).await.unwrap();
        let (range, rows) = recovered.into_parts();
        self.files
            .cache_diagnostic_evidence(RestoredInput {
                store_id: self.store.id().into(),
                parent: capture_reference(range.manifest()).unwrap(),
                source: range.manifest().source.clone(),
                connection: "fixture".into(),
                rows,
            })
            .unwrap();
    }
}

#[tokio::test]
async fn archived_metadata_stays_bounded_across_restarts_without_closing_an_unknown_older_input() {
    let mut fixture = Fixture::new().await;
    let (older, older_commit) = fixture.native(vec![], 0).await;
    let mut first_replay = None;
    let mut latest_replay = None;
    for cycle in 0..72 {
        let (input, native) = fixture.native(vec![], 1).await;
        let update = update(&input, vec![], vec![], &format!("quiet-{cycle}"));
        let derived = fixture.derive(input.clone(), update.clone(), true).await;
        assert!(fixture.files.evict(&native).unwrap());
        if first_replay.is_none() {
            first_replay = Some((input.clone(), update.clone()));
        }
        latest_replay = Some((input, update, derived.manifest().capture.clone()));
        assert_eq!(fixture.count("steps"), 1, "only the current compact retry receipt remains");
        assert_eq!(fixture.count("inputs"), 2, "the pending older actor and current head input remain");
        assert!(!fixture.files.evict(&older_commit).unwrap());
        if cycle % 12 == 11 {
            fixture = fixture.restarted().await;
        }
    }
    let (input, update, capture) = latest_replay.unwrap();
    assert_eq!(fixture.files.commit_diagnostics(input, update).unwrap(), capture);
    let (input, update) = first_replay.unwrap();
    let before = fixture.files.status(10, None).unwrap().captures;
    assert!(fixture.files.commit_diagnostics(input, update).is_err(), "compacted retries never allocate a duplicate");
    assert_eq!(fixture.files.status(10, None).unwrap().captures, before);
    let full_updates: u32 = fixture
        .files
        .journal
        .conn
        .query_row("SELECT count(*) FROM diagnostic_steps WHERE update_json IS NOT NULL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(full_updates, 0);
    assert!(fixture.store.diagnostic_input_pending(&older.parent, 1).await.unwrap());
    let cold_path = fixture.root.path().join("never-local");
    let mut cold = Outbox::create_remote(&cold_path, OutboxLimits::default(), fixture.store.id()).unwrap();
    cold.recover().await.unwrap();
    let head = fixture.store.diagnostic_heads(None, 1).await.unwrap().heads.remove(0);
    cold.restore_diagnostics(load_diagnostic_checkpoint(&fixture.store, &head).await.unwrap()).await.unwrap();
    cold.restore_diagnostic_input(
        "fixture",
        load_diagnostic_input(&fixture.store, &older_commit.manifest().range_id).await.unwrap(),
        &fixture.store,
    )
    .await
    .unwrap();
    assert_eq!(cold.next_diagnostic_input(1).await.unwrap().unwrap().parent, older.parent);
    assert!(
        older.parent.capture.revision.as_ref().unwrap().owner_epoch.parse::<u64>().unwrap()
            < fixture.owner.fence().epoch()
    );
    let pages: u32 = fixture.files.journal.conn.pragma_query_value(None, "page_count", |row| row.get(0)).unwrap();
    assert!(pages < 256, "quiet archived history reuses bounded SQLite metadata pages");
}

#[tokio::test]
async fn archive_acknowledgement_compacts_only_committed_results_and_preserves_the_current_retry_receipt() {
    let mut fixture = Fixture::new().await;
    let (input, native) = fixture.native(vec![], 0).await;
    let update = update(&input, vec![], vec![], "initial");
    let capture = fixture.files.commit_diagnostics(input.clone(), update.clone()).unwrap();
    let committed = fixture.archive(&capture, false).await;
    let json: Option<String> = fixture
        .files
        .journal
        .conn
        .query_row(
            "SELECT update_json FROM diagnostic_steps WHERE observation_id=?1",
            [&capture.observation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(json.is_some(), "a remote commit without local acknowledgement retains its recovery intent");
    fixture.files.mark_archived(&committed).unwrap();
    assert_eq!(fixture.files.commit_diagnostics(input.clone(), update.clone()).unwrap(), capture);
    assert!(fixture.files.evict(&native).unwrap());
    fixture = fixture.restarted().await;
    assert_eq!(fixture.files.commit_diagnostics(input, update).unwrap(), capture);
    assert_eq!(fixture.count("steps"), 1);
    let json: Option<String> = fixture
        .files
        .journal
        .conn
        .query_row(
            "SELECT update_json FROM diagnostic_steps WHERE observation_id=?1",
            [&capture.observation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(json.is_none());
}

#[tokio::test]
async fn a_stale_pending_proof_cannot_install_work_after_metadata_retirement() {
    let mut fixture = Fixture::new().await;
    let (first, native) = fixture.native(vec![], 0).await;
    fixture.derive(first.clone(), update(&first, vec![], vec![], "first"), true).await;
    assert!(fixture.files.evict(&native).unwrap());
    let stale_generation = super::super::retirement::generation(&fixture.files.journal.conn).unwrap();
    let (second, native_second) = fixture.native(vec![], 1).await;
    fixture.derive(second.clone(), update(&second, vec![], vec![], "second"), true).await;
    assert!(fixture.files.evict(&native_second).unwrap());
    assert!(super::super::retirement::generation(&fixture.files.journal.conn).unwrap() > stale_generation);
    let recovered = load_diagnostic_input(&fixture.store, &native.manifest().range_id).await.unwrap();
    let (range, rows) = recovered.into_parts();
    let restored = RestoredInput {
        store_id: fixture.store.id().into(),
        parent: capture_reference(range.manifest()).unwrap(),
        source: range.manifest().source.clone(),
        connection: "fixture".into(),
        rows,
    };
    assert!(fixture.files.restore_diagnostic_input_at_generation(restored, stale_generation).is_err());
    assert!(fixture.files.next_diagnostic_input(1).unwrap().is_none());
    let Fixture { root, files, store, owner: _owner, .. } = fixture;
    drop(files);
    let mut outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    outbox.recover().await.unwrap();
    outbox
        .restore_diagnostic_input(
            "fixture",
            load_diagnostic_input(&store, &native.manifest().range_id).await.unwrap(),
            &store,
        )
        .await
        .unwrap();
    assert!(
        outbox.next_diagnostic_input(1).await.unwrap().is_none(),
        "authoritative acknowledgement forbids replay after restart"
    );
}

#[tokio::test]
async fn acknowledged_evidence_cache_reclaims_its_charge_once_without_becoming_pending() {
    let mut fixture = Fixture::new().await;
    let (first, native) = fixture.native(vec![activity("42")], 0).await;
    let original = first.rows[0].reference.clone();
    // This evidence-only cache belongs to a separate cold namespace and owns no queued analysis obligation.
    let mut cold =
        Files::create_remote(&fixture.root.path().join("cache"), OutboxLimits::default(), fixture.store.id()).unwrap();
    cold.recover().unwrap();
    let recovered = load_diagnostic_input(&fixture.store, &native.manifest().range_id).await.unwrap();
    let (range, rows) = recovered.into_parts();
    cold.cache_diagnostic_evidence(RestoredInput {
        store_id: fixture.store.id().into(),
        parent: capture_reference(range.manifest()).unwrap(),
        source: range.manifest().source.clone(),
        connection: "fixture".into(),
        rows,
    })
    .unwrap();
    assert!(cold.diagnostic_reference(&original).is_ok());
    assert!(cold.status(1, None).unwrap().charged_bytes > 0);
    assert!(cold.next_diagnostic_input(1).unwrap().is_none());
    let tx = cold.journal.conn.transaction().unwrap();
    super::super::retirement::compact(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(cold.status(1, None).unwrap().charged_bytes, 0);
    let generation = super::super::retirement::generation(&cold.journal.conn).unwrap();
    let tx = cold.journal.conn.transaction().unwrap();
    super::super::retirement::compact(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(cold.status(1, None).unwrap().charged_bytes, 0);
    assert_eq!(super::super::retirement::generation(&cold.journal.conn).unwrap(), generation);
    assert!(
        fixture.store.diagnostic_input_pending(&first.parent, 1).await.unwrap(),
        "cache retirement cannot acknowledge a genuine remote obligation"
    );
}

#[tokio::test]
async fn resolved_but_deferred_work_keeps_original_metadata_until_its_terminal_result_is_archived() {
    let mut fixture = Fixture::new().await;
    let (input, native) = fixture.native(vec![activity("42")], 0).await;
    let mut initial = start(&input);
    let mut episode = initial.checkpoint.episodes[0].clone();
    episode.analysis = DiagnosticAnalysisStatus::Deferred;
    episode.issue = Some(DiagnosticAnalysisIssue::CostUnqualified);
    initial = update(&input, vec![episode.clone()], vec![None], "deferred");
    fixture.derive(input.clone(), initial.clone(), true).await;
    assert!(fixture.files.evict(&native).unwrap());
    let (empty, empty_native) = fixture.native(vec![], 10).await;
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    episode.revision = "2".into();
    episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: empty.parent.clone() });
    fixture
        .derive(
            empty.clone(),
            update(&empty, vec![episode.clone()], vec![Some("1".into())], "recovered-deferred"),
            true,
        )
        .await;
    assert!(fixture.files.evict(&empty_native).unwrap());
    assert_eq!(fixture.files.diagnostic_checkpoints(1, None, 1).unwrap().checkpoints[0].episodes, [episode.clone()]);
    assert_eq!(
        fixture.metadata_charge(&input.parent.capture.observation_id),
        Some(0),
        "Deferred evidence metadata survives verified artifact eviction"
    );
    fixture.cache(&native).await;
    assert_eq!(fixture.files.diagnostic_reference(&input.rows[0].reference).unwrap().1, input.rows[0].row);
    let original_charge = fixture.metadata_charge(&input.parent.capture.observation_id).unwrap();
    assert!(original_charge > 0);
    let replay = fixture.files.diagnostic_input(&input.parent.capture.observation_id, 1).unwrap();
    let captures = fixture.files.status(1, None).unwrap().captures;
    assert!(
        fixture.files.commit_diagnostics(replay, initial).is_err(),
        "retained first evidence cannot readmit a compacted initial revision"
    );
    assert_eq!(fixture.files.status(1, None).unwrap().captures, captures);
    fixture.cache(&empty_native).await;
    let current_charge = fixture.metadata_charge(&empty.parent.capture.observation_id).unwrap();
    assert!(current_charge > 0);
    let current = fixture.files.diagnostic_input(&empty.parent.capture.observation_id, 1).unwrap();
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.revision = "3".into();
    let terminal = fixture
        .derive(current.clone(), update(&current, vec![episode], vec![Some("2".into())], "terminal"), false)
        .await;
    assert!(
        fixture.files.diagnostic_input(&input.parent.capture.observation_id, 1).is_ok(),
        "the unarchived terminal result still owns its native inputs"
    );
    fixture.files.mark_archived(&terminal).unwrap();
    assert_eq!(fixture.metadata_charge(&input.parent.capture.observation_id), None);
    assert_eq!(fixture.metadata_charge(&empty.parent.capture.observation_id), Some(current_charge));
    assert_eq!(super::super::journal::charged_bytes(&fixture.files.journal.conn).unwrap(), current_charge);
    assert!(fixture.files.diagnostic_checkpoints(1, None, 1).unwrap().checkpoints[0].episodes.is_empty());
}

#[tokio::test]
async fn interruption_of_archive_retirement_rolls_back_acknowledgement_and_retries_once_after_restart() {
    let mut fixture = Fixture::new().await;
    let (first, native) = fixture.native(vec![], 0).await;
    fixture.derive(first.clone(), update(&first, vec![], vec![], "first"), true).await;
    assert!(fixture.files.evict(&native).unwrap());
    let (next, native_next) = fixture.native(vec![], 1).await;
    // Keep the previous head until this derived result seals, then hold its archive acknowledgement.
    let derived = fixture.derive(next.clone(), update(&next, vec![], vec![], "next"), false).await;
    assert!(fixture.files.evict(&native_next).unwrap());
    // Cache an unreferenced authenticated older row so this transaction must retire metadata.
    let recovered = load_diagnostic_input(&fixture.store, &native.manifest().range_id).await.unwrap();
    let (range, rows) = recovered.into_parts();
    fixture
        .files
        .cache_diagnostic_evidence(RestoredInput {
            store_id: fixture.store.id().into(),
            parent: capture_reference(range.manifest()).unwrap(),
            source: range.manifest().source.clone(),
            connection: "fixture".into(),
            rows,
        })
        .unwrap();
    let before = fixture.files.status(1, None).unwrap().charged_bytes;
    let generation = super::super::retirement::generation(&fixture.files.journal.conn).unwrap();
    fixture.files.journal.conn.execute_batch("CREATE TRIGGER stop_retirement BEFORE DELETE ON diagnostic_inputs BEGIN SELECT RAISE(ABORT,'test-stop'); END").unwrap();
    assert!(fixture.files.mark_archived(&derived).is_err());
    assert_eq!(fixture.files.archive_range(&derived.manifest().capture.observation_id).unwrap(), None);
    assert_eq!(fixture.files.status(1, None).unwrap().charged_bytes, before);
    assert_eq!(super::super::retirement::generation(&fixture.files.journal.conn).unwrap(), generation);
    fixture.files.journal.conn.execute_batch("DROP TRIGGER stop_retirement").unwrap();
    fixture = fixture.restarted().await;
    fixture.files.mark_archived(&derived).unwrap();
    fixture.files.mark_archived(&derived).unwrap();
    assert_eq!(fixture.files.journal.debts().unwrap(), 0);
    assert_eq!(super::super::journal::charged_bytes(&fixture.files.journal.conn).unwrap(), 0);
}
