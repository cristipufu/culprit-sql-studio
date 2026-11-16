use super::*;
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{capture_reference, load_diagnostic_input},
    file_store::{FileOwnership, FileRecoveryStore},
    recovery::{CommittedRange, RecoveryStore, stage_and_commit},
};
use crate::outbox::investigations::{RestoredInput, retirement};

struct ArchivedFocus {
    root: tempfile::TempDir,
    files: Files,
    store: FileRecoveryStore,
    owner: FileOwnership,
    contract: Contract,
    head: Option<String>,
    capture: CaptureIdentity,
}

impl ArchivedFocus {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let contract = Contract::new("pipeline".into(), "shard".into(), "file:test".into(), digest(b"schema")).unwrap();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut files = Files::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
        files.recover().unwrap();
        files.begin_remote_epoch(owner.fence(), 0).unwrap();
        let request = qualified_request(&mut files, compiler_admission_investigation_fixture());
        let evidence = compiler_admission_evidence();
        let (source, records) = focused_source(&request, evidence.0, evidence.1);
        let capture = seal(&mut files, request, source, records);
        Self { root, files, store, owner, contract, head: None, capture }
    }

    async fn archive(&mut self, capture: &CaptureIdentity) -> CommittedRange {
        let prepared = self.files.prepare_delivery(&capture.observation_id, self.contract.clone(), vec![]).unwrap();
        let committed =
            stage_and_commit(&self.store, self.owner.fence(), self.head.as_deref(), &prepared).await.unwrap();
        self.head = Some(committed.manifest().range_id.clone());
        committed
    }

    async fn restored(&self, committed: &CommittedRange, connection: &str) -> RestoredInput {
        let (range, rows) =
            load_diagnostic_input(&self.store, &committed.manifest().range_id).await.unwrap().into_parts();
        RestoredInput {
            store_id: self.store.id().into(),
            parent: capture_reference(range.manifest()).unwrap(),
            source: range.manifest().source.clone(),
            investigation: range.manifest().investigation.clone(),
            connection: connection.into(),
            rows,
        }
    }

    fn cold(&self, name: &str, max_bytes: u64) -> Files {
        let mut files = Files::create_remote(
            &self.root.path().join(name),
            OutboxLimits { max_bytes, ..OutboxLimits::default() },
            self.store.id(),
        )
        .unwrap();
        files.recover().unwrap();
        files
    }

    async fn consume(&mut self, capture: &CaptureIdentity, step: &str) {
        let input = self.files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        let update = investigations::DiagnosticUpdate {
            step_id: digest(step.as_bytes()),
            analyzed_at: "2020-01-01T12:00:02Z".into(),
            revisions: vec![],
            checkpoint: DiagnosticCheckpoint {
                controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
                target: input.controller_target().unwrap().clone(),
                input: input.parent.clone(),
                episodes: vec![],
            },
        };
        let derived = self.files.commit_diagnostics(input, update).unwrap();
        let committed = self.archive(&derived).await;
        self.files.mark_archived(&committed).unwrap();
    }
}

fn metadata_charge(files: &Files, capture: &CaptureIdentity) -> (u64, u64) {
    let (charge, bytes): (i64, i64) = files
        .journal
        .conn
        .query_row(
            "SELECT charged_bytes,length(CAST(parent_json AS BLOB))+length(CAST(source_json AS BLOB))+length(CAST(connection AS BLOB))+coalesce(length(CAST(lineage_json AS BLOB)),0) FROM diagnostic_inputs WHERE observation_id=?1",
            [&capture.observation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    (u64::try_from(charge).unwrap(), u64::try_from(bytes).unwrap())
}

fn restored_charge(input: &RestoredInput) -> u64 {
    CAPTURE_ACCOUNTING
        + serde_json::to_vec(&input.parent).unwrap().len() as u64
        + serde_json::to_vec(&input.source).unwrap().len() as u64
        + input.connection.len() as u64
        + input.investigation.as_ref().map_or(0, |owner| serde_json::to_vec(owner).unwrap().len()) as u64
        + serde_json::to_vec(&input.rows).unwrap().len() as u64
}

#[tokio::test]
async fn a_long_focus_keeps_its_exact_base_charge_after_eviction_and_adds_restored_rows_only_once() {
    let mut fixture = ArchivedFocus::new().await;
    let capture = fixture.capture.clone();
    let (base_charge, base_bytes) = metadata_charge(&fixture.files, &capture);
    assert!(base_bytes > ORIGINAL_SQL_BYTES as u64);
    assert_eq!(base_charge, base_bytes + CAPTURE_ACCOUNTING);
    let native = fixture.archive(&capture).await;
    fixture.consume(&capture, "consume focused compiler evidence").await;
    assert!(fixture.files.evict(&native).unwrap());
    assert_eq!(metadata_charge(&fixture.files, &capture), (base_charge, base_bytes));
    let restored = fixture.restored(&native, "a-new-local-alias-with-a-different-length").await;
    let rows_bytes = serde_json::to_vec(&restored.rows).unwrap().len() as u64;
    fixture.files.cache_diagnostic_evidence(restored.clone()).unwrap();
    assert_eq!(metadata_charge(&fixture.files, &capture), (base_charge + rows_bytes, base_bytes));
    fixture.files.cache_diagnostic_evidence(restored).unwrap();
    assert_eq!(metadata_charge(&fixture.files, &capture), (base_charge + rows_bytes, base_bytes));
    let connection: String = fixture
        .files
        .journal
        .conn
        .query_row(
            "SELECT connection FROM diagnostic_inputs WHERE observation_id=?1",
            [&capture.observation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(connection, "fixture", "a cache upgrade retains its original persisted base metadata");
    let request = qualified_request(&mut fixture.files, compiler_admission_investigation_fixture());
    let successor = request.focus().unwrap().parent.capture.clone();
    // The derived checkpoint's parent must already have its own authenticated recovery commit.
    fixture.archive(&successor).await;
    fixture.consume(&successor, "advance the compact checkpoint").await;
    let remaining: u32 = fixture
        .files
        .journal
        .conn
        .query_row("SELECT count(*) FROM diagnostic_inputs WHERE observation_id=?1", [&capture.observation_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0, "retirement deletes the retained base and restored charge together");
}

#[tokio::test]
async fn cold_restoration_charges_all_retained_copies_before_insertion_at_the_exact_quota_boundary() {
    let mut fixture = ArchivedFocus::new().await;
    let capture = fixture.capture.clone();
    let native = fixture.archive(&capture).await;
    let input = fixture.restored(&native, "restored-alias-é").await;
    let input_charge = restored_charge(&input);
    let filler = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget { page_bytes: 1024, total_bytes: 1024 * 1024, ..Default::default() },
    };
    filler.validate().unwrap();
    let reservation_charge =
        u64::from(filler.budget.total_bytes) + CAPTURE_ALLOWANCE + serde_json::to_vec(&filler).unwrap().len() as u64;
    for admitted in [false, true] {
        let name = if admitted { "exact-quota" } else { "short-quota" };
        let max_bytes = reservation_charge + input_charge - u64::from(!admitted);
        let mut cold = fixture.cold(name, max_bytes);
        cold.begin_remote_epoch(fixture.owner.fence(), 2).unwrap();
        let reservation = cold.reserve("bounded reserved metadata", &filler).unwrap();
        let before = cold.status(10, None).unwrap().charged_bytes;
        assert_eq!(before, reservation_charge);
        match cold.cache_diagnostic_evidence(input.clone()) {
            Ok(()) => {
                assert!(admitted);
                assert_eq!(cold.status(10, None).unwrap().charged_bytes, max_bytes);
                assert_eq!(metadata_charge(&cold, &capture).0, input_charge);
            }
            Err(error) => {
                assert!(!admitted);
                assert_eq!(kind(&error), OutboxProblem::Capacity);
                assert_eq!(cold.status(10, None).unwrap().charged_bytes, before);
                let rows: u32 = cold
                    .journal
                    .conn
                    .query_row("SELECT count(*) FROM diagnostic_inputs", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(rows, 0);
            }
        }
        assert!(!reservation.directory.exists());
        assert_eq!(
            cold.journal.capture(&reservation.entry.identity.observation_id).unwrap().source_work,
            SourceWorkState::NotStarted
        );
    }
}

#[tokio::test]
async fn interrupted_retirement_keeps_every_long_metadata_byte_and_retry_releases_it_exactly_once() {
    let mut fixture = ArchivedFocus::new().await;
    let capture = fixture.capture.clone();
    let native = fixture.archive(&capture).await;
    let input = fixture.restored(&native, "cached-alias").await;
    let expected = restored_charge(&input);
    let mut cold = fixture.cold("cold", OutboxLimits::default().max_bytes);
    cold.cache_diagnostic_evidence(input).unwrap();
    assert_eq!(cold.status(1, None).unwrap().charged_bytes, expected);
    let generation = retirement::generation(&cold.journal.conn).unwrap();
    cold.journal.conn.execute_batch("CREATE TRIGGER stop_retirement BEFORE DELETE ON diagnostic_inputs BEGIN SELECT RAISE(ABORT,'test-stop'); END").unwrap();
    {
        let tx = cold.journal.conn.transaction().unwrap();
        assert!(retirement::compact(&tx).is_err());
    }
    assert_eq!(cold.status(1, None).unwrap().charged_bytes, expected);
    assert_eq!(retirement::generation(&cold.journal.conn).unwrap(), generation);
    cold.journal.conn.execute_batch("DROP TRIGGER stop_retirement").unwrap();
    for _ in 0..2 {
        let tx = cold.journal.conn.transaction().unwrap();
        retirement::compact(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(cold.status(1, None).unwrap().charged_bytes, 0);
        assert_eq!(retirement::generation(&cold.journal.conn).unwrap(), generation + 1);
    }
}
