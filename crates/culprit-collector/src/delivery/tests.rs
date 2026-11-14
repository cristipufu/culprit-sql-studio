use super::{contract::*, evidence::*, file_store::*, recovery::*, sink::*};
use crate::collection_files::ObservationFiles;
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::sync::Mutex;
pub(super) fn contract() -> Contract {
    Contract::new("pipeline".into(), "shard-0".into(), "file:warehouse".into(), digest(b"schema")).unwrap()
}
pub(super) fn prepared(root: &std::path::Path, sequence: u64) -> PreparedRange {
    prepared_database(root, sequence, "db")
}
fn prepared_database(root: &std::path::Path, sequence: u64, database: &str) -> PreparedRange {
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::ResourceHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
        },
        budget: CollectionBudget::default(),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            database,
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "resources".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
            observed_at: "2020-01-01T06:02:00Z".into(),
            available_from: Some("2020-01-01T05:00:00Z".into()),
            available_until: Some("2020-01-01T06:01:30Z".into()),
            expected_samples: 1,
            nominal_period_ms: Some(15000),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: format!("observation-{sequence}"),
        revision: Some(CaptureRevision {
            outbox_id: "outbox".into(),
            owner_epoch: "1".into(),
            sequence: sequence.to_string(),
        }),
    };
    let directory = root.join(&capture.observation_id);
    let mut writer = ObservationFiles::create(&directory, request, capture).unwrap();
    writer.describe(source).unwrap();
    let row = TelemetryRecord::ResourceSample(ResourceMeasurement {
        at: "2020-01-01T06:00:15Z".into(),
        replica_id: Some("primary".into()),
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            unit: ResourceUnit::Percent,
            scope: culprit_core::MeasurementScope::Database,
            statistic: ResourceStatistic::Mean,
            period_ms: Some(15000),
            limit_basis: ResourceLimitBasis::DatabaseLimit,
            value: Some(12.5),
        }],
    });
    assert!(writer.write_page(0, &[row]).unwrap().is_none());
    writer.seal(0).unwrap();
    writer.confirm_source(culprit_core::telemetry::SourceCompletion::Verified).unwrap();
    let manifest = writer.finish(1).unwrap();
    PreparedRange::from_validated(
        &directory,
        &manifest,
        contract(),
        vec![StreamAdvance {
            stream_key: "db/resource-history".into(),
            previous: (sequence > 1).then(|| (sequence - 1).to_string()),
            next: sequence.to_string(),
        }],
    )
    .unwrap()
}
#[tokio::test]
async fn pending_commit_order_is_independent_of_how_many_distinct_native_ranges_have_been_indexed() {
    let root = tempfile::tempdir().unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut head = None;
    let mut expected = vec![];
    for (sequence, database) in [(1, "db"), (2, "db"), (3, "other-database")] {
        let prepared = prepared_database(root.path(), sequence, database);
        let committed = stage_and_commit(&store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
        head = Some(committed.manifest().range_id.clone());
        expected.push(committed.manifest().range_id.clone());
    }
    let mut actual = vec![];
    let mut cursor = None;
    loop {
        let page = store.pending_ranges(cursor.as_deref(), 1, RecoveryOrder::OldestFirst).await.unwrap();
        actual.extend(page.ranges.into_iter().map(|range| range.range_id));
        let Some(next) = page.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    assert_eq!(actual, expected);
}
pub(super) fn problem(error: culprit_core::Error) -> DeliveryProblem {
    match error.detail {
        Some(culprit_core::ErrorDetail::Delivery(problem)) => problem,
        _ => panic!("typed delivery failure"),
    }
}
#[tokio::test]
async fn payload_commit_checkpoint_reopen_and_replay_keep_original_membership() {
    let temp = tempfile::TempDir::new().unwrap();
    let prepared = prepared(temp.path(), 1);
    let store = FileRecoveryStore::create(&temp.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let again = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    assert_eq!(committed.manifest_digest(), again.manifest_digest());
    assert_eq!(store.checkpoint("db/resource-history").await.unwrap(), Some("1".into()));
    let batch = &committed.manifest().batches[0];
    let mut records = RecoveryRecords::open(&store, batch).await.unwrap();
    let mut restored = Vec::new();
    while let Some(row) = records.next().await.unwrap() {
        restored.push(row);
    }
    let original = std::fs::read_to_string(prepared.batch_path(batch))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<culprit_core::telemetry::ExportedRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(restored, original);
    drop(owner);
    drop(store);
    let store = FileRecoveryStore::open(&temp.path().join("recovery"), contract()).unwrap();
    let recovered = reopen(&store, &committed.manifest().range_id).await.unwrap();
    assert_eq!(recovered.manifest(), committed.manifest());
    let page = store.recover(None, 1).await.unwrap();
    assert_eq!(page.ranges.len(), 1);
    assert_eq!(page.high_sequence, 1);
    assert!(page.next_cursor.is_none());
}
#[tokio::test]
async fn stale_owners_and_wrong_checkpoints_cannot_advance_recovery() {
    let temp = tempfile::TempDir::new().unwrap();
    let first = prepared(temp.path(), 1);
    let second = prepared(temp.path(), 2);
    let store = FileRecoveryStore::create(&temp.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let stale = owner.fence().clone();
    drop(owner);
    let owner = store.acquire().await.unwrap();
    assert_eq!(problem(stage_and_commit(&store, &stale, None, &first).await.err().unwrap()), DeliveryProblem::Fenced);
    assert!(store.checkpoint_head().await.unwrap().is_none());
    assert_eq!(
        problem(stage_and_commit(&store, owner.fence(), None, &second).await.err().unwrap()),
        DeliveryProblem::Conflict
    );
    let first = stage_and_commit(&store, owner.fence(), None, &first).await.unwrap();
    assert_eq!(
        problem(stage_and_commit(&store, owner.fence(), None, &second).await.err().unwrap()),
        DeliveryProblem::Conflict
    );
    stage_and_commit(&store, owner.fence(), Some(&first.manifest().range_id), &second).await.unwrap();
    let first_page = store.recover(None, 1).await.unwrap();
    let second_page = store.recover(first_page.next_cursor.as_deref(), 1).await.unwrap();
    assert_eq!(second_page.ranges[0].capture.revision.as_ref().unwrap().sequence, "2");
    assert_eq!(second_page.high_sequence, 2);
}
#[tokio::test]
async fn corrupt_payloads_and_changed_destinations_never_become_dispatchable() {
    let temp = tempfile::TempDir::new().unwrap();
    let prepared = prepared(temp.path(), 1);
    let root = temp.path().join("recovery");
    let store = FileRecoveryStore::create(&root, contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    std::fs::write(prepared.batch_path(&prepared.manifest().batches[0]), b"corrupt\n").unwrap();
    assert_eq!(
        problem(stage_and_commit(&store, owner.fence(), None, &prepared).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    assert!(store.recover(None, 10).await.unwrap().ranges.is_empty());
    let other = Contract::new("pipeline".into(), "shard-0".into(), "file:other".into(), digest(b"schema")).unwrap();
    assert_eq!(problem(FileRecoveryStore::open(&root, other).err().unwrap()), DeliveryProblem::DestinationChanged);
}

struct ScriptedSink {
    pages: Mutex<std::collections::VecDeque<ReceiptPage>>,
}
#[async_trait]
impl TelemetrySink for ScriptedSink {
    async fn validate_contract(&self, _: &Contract) -> Result<()> {
        Ok(())
    }
    async fn dispatch(&self, _: &CommittedRange, _: &DeliveryAttempt, _: &str) -> Result<Submission> {
        Ok(Submission::Unknown)
    }
    async fn receipts(&self, _: &CommittedRange, _: Option<&str>, _: u32) -> Result<ReceiptPage> {
        Ok(self.pages.lock().unwrap().pop_front().expect("bounded verification"))
    }
    async fn publish(&self, range: &VerifiedRange, _: &DeliveryAttempt) -> Result<Publication> {
        Ok(Publication {
            range_id: range.committed().manifest().range_id.clone(),
            manifest_digest: range.committed().manifest_digest().into(),
        })
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        Ok(publication.range_id == range.manifest().range_id && publication.manifest_digest == range.manifest_digest())
    }
    async fn poll_updates(&self, _: Option<&str>, _: u32) -> Result<DeliveryUpdates> {
        Ok(DeliveryUpdates { updates: vec![], next_cursor: "0".into(), expired: false })
    }
}
fn sink(rows: Vec<RowReceipt>) -> ScriptedSink {
    ScriptedSink { pages: Mutex::new([ReceiptPage { rows, next_cursor: None, complete: true }].into()) }
}
#[tokio::test]
async fn equal_counts_with_substituted_or_conflicting_rows_cannot_authorize_publication() {
    let temp = tempfile::TempDir::new().unwrap();
    let prepared = prepared(temp.path(), 1);
    let store = FileRecoveryStore::create(&temp.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let receipts = committed.manifest().expected_receipts().cloned().collect::<Vec<_>>();
    let mut substituted = receipts.clone();
    substituted[0].key.record_id = digest(b"unexpected");
    assert_eq!(
        problem(verify(&sink(substituted), &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    let mut conflicting = receipts.clone();
    conflicting[0].payload_digest = digest(b"wrong mapping");
    assert_eq!(
        problem(verify(&sink(conflicting), &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    assert_eq!(
        problem(verify(&sink(vec![]), &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::NotReady
    );
    let mut duplicates = receipts.clone();
    duplicates.extend(receipts);
    let sink = sink(duplicates);
    let verified = verify(&sink, &committed, VerificationBudget::default()).await.unwrap();
    let publication = sink.publish(&verified, &DeliveryAttempt::new(1).unwrap()).await.unwrap();
    assert!(sink.publication_visible(&publication, &committed).await.unwrap());
}
#[tokio::test]
async fn incomplete_queries_mapping_nulls_and_repeated_cursors_withhold_publication() {
    let temp = tempfile::TempDir::new().unwrap();
    let prepared = prepared(temp.path(), 1);
    let store = FileRecoveryStore::create(&temp.path().join("recovery"), contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let mut receipts = committed.manifest().expected_receipts().cloned().collect::<Vec<_>>();
    receipts[0].critical_fields_valid = false;
    assert_eq!(
        problem(verify(&sink(receipts), &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    let sink =
        ScriptedSink { pages: Mutex::new([ReceiptPage { rows: vec![], next_cursor: None, complete: false }].into()) };
    assert_eq!(
        problem(verify(&sink, &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
    let page = ReceiptPage { rows: vec![], next_cursor: Some("same".into()), complete: true };
    let sink = ScriptedSink { pages: Mutex::new([page.clone(), page].into()) };
    assert_eq!(
        problem(verify(&sink, &committed, VerificationBudget::default()).await.err().unwrap()),
        DeliveryProblem::Integrity
    );
}

#[tokio::test]
async fn a_staged_payload_lost_before_commit_cannot_advance_the_recovery_checkpoint() {
    let home = tempfile::tempdir().unwrap();
    let prepared = prepared(home.path(), 1);
    let directory = home.path().join("recovery");
    let store = FileRecoveryStore::create(&directory, contract()).unwrap();
    let owner = store.acquire().await.unwrap();
    let batch = &prepared.manifest().batches[0];
    let staged = store.stage(&PayloadObject::from(batch), &prepared.batch_path(batch)).await.unwrap();
    std::fs::remove_file(directory.join(&batch.object)).unwrap();
    assert!(store.commit(owner.fence(), None, prepared.manifest(), &[staged]).await.is_err());
    assert!(store.checkpoint_head().await.unwrap().is_none());
    assert!(store.checkpoint("db/resource-history").await.unwrap().is_none());
}

#[test]
fn expected_native_receipt_identity_and_representation_must_match_the_actual_stream() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared(root.path(), 1);
    let original = prepared.manifest().clone();
    let mut changed = original.clone();
    changed.batches[0].rows[0].native_key =
        NativeRecordIdentity::Operational { source: "other-source".into(), key: "other-key".into() };
    assert_eq!(problem(changed.validate().unwrap_err()), DeliveryProblem::Integrity);
    changed = original.clone();
    changed.batches[0].rows[0].native_key =
        NativeRecordIdentity::ResourceSample { at: "not-a-native-UTC-instant".into(), replica_id: None };
    assert_eq!(problem(changed.validate().unwrap_err()), DeliveryProblem::Integrity);
    changed = original;
    changed.batches[0].rows[0].key.representation_version = REPRESENTATION_VERSION + 1;
    assert_eq!(problem(changed.validate().unwrap_err()), DeliveryProblem::Integrity);
}
