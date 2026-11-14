use super::*;
use crate::collection_files::ObservationFiles;
mod delivery_worker;
mod eviction;
mod restored_activity;

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: culprit_core::telemetry::CollectionSelection::QueryHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        budget: CollectionBudget::default(),
    }
}
fn source() -> CollectionDescription {
    let mut database =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "fixture", "fixture.invalid", 1433, None);
    database.incarnation = Some("native-fixture-guid".into());
    CollectionDescription {
        target: database.into(),
        source: "query_store".into(),
        observation: culprit_core::telemetry::SourceObservation::QueryHistory(
            culprit_core::telemetry::QueryHistoryDescription {
                waits: None,
                interval: TelemetryInterval {
                    native_id: "8".into(),
                    from: request().bounds().unwrap().0.to_rfc3339(),
                    until: request().bounds().unwrap().1.to_rfc3339(),
                },
                dimensions: [("replica_group_id".into(), vec!["1".into()])].into(),
                native_properties: [
                    ("capture_mode".into(), "AUTO".into()),
                    ("waits_capture_enabled".into(), (true).to_string()),
                ]
                .into(),
            },
        ),
        continuity: SourceContinuity::Unverified,
    }
}
fn plan() -> NativePlanIdentity {
    NativePlanIdentity {
        plan_id: "7".into(),
        query: NativeQueryIdentity {
            query_id: "2".into(),
            qualifiers: [
                ("context_settings_id".into(), "3".into()),
                ("object_id".into(), "0".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
            ]
            .into(),
        },
        qualifiers: [
            ("plan_hash".into(), "FEDCBA9876543210".into()),
            ("initial_compile_at".into(), "2020-01-01T05:00:01Z".into()),
        ]
        .into(),
    }
}
fn records(stream: TelemetryStream) -> Vec<TelemetryRecord> {
    let identity = plan();
    let source = source();
    match stream {
        TelemetryStream::QueryRuntime => vec![TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new("1")
                    .qualified("plan_id", identity.plan_id.clone())
                    .qualified("replica_group_id", "1")
                    .qualified("execution_type", "0".to_string()),
                references: StatementReferences {
                    query: Some(NativeDefinitionReference {
                        native: identity.query.native(),
                        definition_id: Some(identity.query.definition_id(&source).unwrap()),
                    }),
                    plan: Some(NativeDefinitionReference {
                        native: identity.native(),
                        definition_id: Some(identity.definition_id(&source).unwrap()),
                    }),
                },
            },
            executions: "9007199254740993".into(),
            metrics: vec![
                RuntimeMetric {
                    minimum: Some(1.0),
                    maximum: Some(3.0),
                    standard_deviation: Some(1.0),
                    ..RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 2.0)
                },
                RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Microseconds, 1.0),
                RuntimeMetric::average(
                    RuntimeMetricKey::LogicalReads,
                    RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                    1.0,
                ),
                RuntimeMetric::average(
                    RuntimeMetricKey::PhysicalReads,
                    RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
                    0.0,
                ),
            ],
            first_execution_at: Some("2020-01-01T06:01:00Z".into()),
            last_execution_at: Some("2020-01-01T06:59:00Z".into()),
        })],
        TelemetryStream::QueryWaits => vec![],
        TelemetryStream::QueryDefinitions => vec![TelemetryRecord::QueryDefinition(QueryDefinition {
            variant_family: None,
            origin: culprit_core::telemetry::StatementOrigin::Unknown,
            monitoring_template_id: None,
            id: identity.query.definition_id(&source).unwrap(),
            identity: identity.query,
            context: Default::default(),
            context_available: true,
            source_text_bytes: Some("100".into()),
            sql: DefinitionPayload::sql("SELECT N'private-value'", culprit_core::Engine::Mssql),
        })],
        TelemetryStream::DeadlockEvents
        | TelemetryStream::OperationalSamples
        | TelemetryStream::WaitCounterSamples
        | TelemetryStream::ReplicationSamples
        | TelemetryStream::SettingObservations
        | TelemetryStream::TuningObservations
        | TelemetryStream::ObjectDefinitions
        | TelemetryStream::ResourceSamples
        | TelemetryStream::ActivityObservations
        | TelemetryStream::BlockingEdges
        | TelemetryStream::ActivityIncidents
        | TelemetryStream::ActivityCheckpoints => panic!("query-history fixture does not collect this stream"),
        TelemetryStream::PlanDefinitions => vec![TelemetryRecord::PlanDefinition(PlanDefinition {
            identity: identity.clone(),
            id: identity.definition_id(&source).unwrap(),
            query_definition_id: Some(identity.query.definition_id(&source).unwrap()),
            source_text_bytes: Some("200".into()),
            plan: DefinitionPayload::plan(
                "<ShowPlanXML><StmtSimple StatementText=\"SELECT 'private-value'\" /></ShowPlanXML>",
                culprit_core::Engine::Mssql,
            ),
            engine_version: Some("16.0".into()),
            native_properties: [("compatibility_level".into(), "160".to_string())].into(),
        })],
    }
}
async fn sealed(lease: &mut CaptureLease<'_>) -> CollectionManifest {
    let mut files = ObservationFiles::create(lease.directory(), request(), lease.identity().clone()).unwrap();
    lease.source_started().await.unwrap();
    files.describe(source()).unwrap();
    for (index, stream) in request().collection().streams().iter().copied().enumerate() {
        files.begin_page(index).unwrap();
        assert!(files.write_page(index, &records(stream)).unwrap().is_none());
        files.seal(index).unwrap();
    }
    files.confirm_source(culprit_core::telemetry::SourceCompletion::Verified).unwrap();
    lease.source_closed(SourceShutdown::Quiescent).await.unwrap();
    files.finish(0).unwrap()
}
async fn complete(outbox: &mut Outbox) -> CollectionManifest {
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    let manifest = sealed(&mut lease).await;
    lease.finish().await.unwrap();
    manifest
}
fn box_at(temp: &tempfile::TempDir) -> (PathBuf, Outbox) {
    let path = temp.path().join("outbox");
    let outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    (path, outbox)
}
async fn journal_fault(outbox: &Outbox, sql: &'static str) {
    outbox
        .files
        .run(|| {
            move |files| {
                files.journal.conn.execute_batch(sql).unwrap();
                Ok(())
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn delivery_prepares_sealed_data_during_capture_and_retains_the_single_filesystem_owner() {
    let root = tempfile::TempDir::new().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    outbox.recover().await.unwrap();
    let first = complete(&mut outbox).await;
    let delivery = outbox.delivery();
    let mut active = outbox.reserve("fixture", &request()).await.unwrap();
    let status = delivery.status(50, None).await.unwrap();
    assert_eq!(status.captures, 2);
    assert_eq!(status.ready, 1);
    let contract = crate::delivery::contract::Contract::new(
        "pipeline".into(),
        "shard-0".into(),
        "file:test".into(),
        digest(b"schema"),
    )
    .unwrap();
    let prepared = delivery.prepare(&first.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    assert_eq!(prepared.manifest().capture, first.capture);
    assert!(delivery.prepare(&active.identity().observation_id, contract, vec![]).await.is_err());
    sealed(&mut active).await;
    active.finish().await.unwrap();
    drop(outbox);
    assert_eq!(
        Outbox::open(&path).err().unwrap().detail,
        Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Locked))
    );
    drop(delivery);
    Outbox::open(&path).unwrap();
}

#[tokio::test]
async fn volume_loss_preserves_the_remote_namespace_and_advances_epoch_and_sequence_before_new_capture() {
    use crate::delivery::{
        contract::Contract,
        file_store::FileRecoveryStore,
        recovery::{RecoveryStore, stage_and_commit},
    };
    let remote = tempfile::TempDir::new().unwrap();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&remote.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let original = tempfile::TempDir::new().unwrap();
    let mut outbox =
        Outbox::create_remote(&original.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    assert_eq!(kind(&outbox.reserve("fixture", &request()).await.err().unwrap()), OutboxProblem::RecoveryRequired);
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let first = complete(&mut outbox).await;
    let prepared = outbox.delivery().prepare(&first.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    drop(prepared);
    drop(outbox);
    drop(original);
    drop(owner);
    let replacement = tempfile::TempDir::new().unwrap();
    let owner = store.acquire().await.unwrap();
    let recovered = store.recover(None, 1).await.unwrap();
    let mut outbox =
        Outbox::create_remote(&replacement.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), recovered.high_sequence).await.unwrap();
    let second = complete(&mut outbox).await;
    let a = first.capture.revision.unwrap();
    let b = second.capture.revision.unwrap();
    assert_eq!(a.outbox_id, b.outbox_id);
    assert_eq!((a.owner_epoch.as_str(), a.sequence.as_str()), ("1", "1"));
    assert_eq!((b.owner_epoch.as_str(), b.sequence.as_str()), ("2", "2"));
}

#[tokio::test]
async fn remote_epoch_activation_refuses_wrong_store_reused_owners_and_unconfirmed_source_work() {
    use crate::delivery::recovery::Fence;
    let root = tempfile::TempDir::new().unwrap();
    let store_id = digest(b"recovery store");
    let fence = Fence::issued(7, "owner".into(), store_id.clone()).unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create_remote(&path, OutboxLimits::default(), &store_id).unwrap();
    outbox.recover().await.unwrap();
    let wrong = Fence::issued(7, "owner".into(), digest(b"other store")).unwrap();
    assert!(outbox.begin_remote_epoch(&wrong, 80).await.is_err());
    outbox.begin_remote_epoch(&fence, 80).await.unwrap();
    assert!(outbox.begin_remote_epoch(&fence, 80).await.is_err());
    let first = complete(&mut outbox).await;
    assert_eq!(first.capture.revision.unwrap().sequence, "81");
    drop(outbox);
    let mut outbox = Outbox::open(&path).unwrap();
    outbox.recover().await.unwrap();
    assert!(outbox.begin_remote_epoch(&fence, 1).await.is_err());
    let next = Fence::issued(8, "next-owner".into(), store_id.clone()).unwrap();
    outbox.begin_remote_epoch(&next, 1).await.unwrap();
    let mut active = outbox.reserve("fixture", &request()).await.unwrap();
    assert_eq!(active.identity().revision.as_ref().unwrap().sequence, "82");
    active.source_started().await.unwrap();
    drop(active);
    drop(outbox);
    let mut outbox = Outbox::open(&path).unwrap();
    let report = outbox.recover().await.unwrap();
    assert_eq!(report.source_debts, 1);
    let future = Fence::issued(9, "future-owner".into(), store_id).unwrap();
    assert_eq!(kind(&outbox.begin_remote_epoch(&future, 82).await.err().unwrap()), OutboxProblem::SourceUnconfirmed);
}

#[tokio::test]
async fn verified_remote_eviction_reclaims_payload_and_metadata_quota_without_reusing_revisions() {
    use crate::delivery::{
        contract::Contract,
        file_store::FileRecoveryStore,
        recovery::{RecoveryStore, stage_and_commit},
    };
    let root = tempfile::TempDir::new().unwrap();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut outbox = Outbox::create_remote(
        &root.path().join("outbox"),
        OutboxLimits { max_captures: 1, ..Default::default() },
        store.id(),
    )
    .unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let mut head = None;
    for sequence in 1..=6 {
        let manifest = complete(&mut outbox).await;
        assert_eq!(manifest.capture.revision.as_ref().unwrap().sequence, sequence.to_string());
        let delivery = outbox.delivery();
        let prepared = delivery.prepare(&manifest.capture.observation_id, contract.clone(), vec![]).await.unwrap();
        let committed = stage_and_commit(&store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
        assert!(delivery.evict(&committed).await.unwrap());
        assert!(delivery.evict(&committed).await.unwrap(), "repeating an acknowledged eviction is harmless");
        head = Some(committed.manifest().range_id.clone());
        let status = outbox.status(50, None).await.unwrap();
        assert_eq!(status.captures, 0);
        assert_eq!(status.charged_bytes, 0);
        let key = manifest.description.unwrap().captured_range().unwrap();
        assert!(outbox.captured_range(&key).await.unwrap().is_none());
        assert!(store.captured_range(&key).await.unwrap().is_some());
        assert!(std::fs::read_dir(root.path().join("outbox/captures")).unwrap().next().is_none());
    }
    drop(outbox);
    Outbox::open(&root.path().join("outbox")).unwrap().recover().await.unwrap();
}

fn kind(error: &Error) -> OutboxProblem {
    match error.detail {
        Some(culprit_core::ErrorDetail::Outbox(kind)) => kind,
        _ => panic!("expected an outbox problem"),
    }
}

#[tokio::test]
async fn revisions_are_ordered_and_allocations_are_not_reused_after_reopening() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let first = complete(&mut outbox).await;
    let second = complete(&mut outbox).await;
    let a = first.capture.revision.unwrap();
    let b = second.capture.revision.unwrap();
    assert_eq!(a.owner_epoch, b.owner_epoch);
    assert_eq!((a.sequence.as_str(), b.sequence.as_str()), ("1", "2"));
    drop(outbox);
    let mut outbox = Outbox::open(&path).unwrap();
    let third = complete(&mut outbox).await.capture.revision.unwrap();
    assert_eq!(third.outbox_id, a.outbox_id);
    assert_eq!(third.owner_epoch, "2");
    assert_eq!(third.sequence, "1");
}
#[tokio::test]
async fn replay_preserves_all_bytes_batches_and_revision_without_allocating_a_capture() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let manifest = complete(&mut outbox).await;
    let before = outbox.status(20, None).await.unwrap();
    let destination = temp.path().join("replayed");
    let replay = Outbox::replay(&path, &manifest.capture.observation_id, &destination).await.unwrap();
    assert_eq!(replay.capture, manifest.capture);
    assert_eq!(replay.batches, 4);
    for name in request()
        .collection()
        .streams()
        .iter()
        .copied()
        .map(|stream| stream.file_name())
        .chain(std::iter::once("manifest.json"))
    {
        assert_eq!(
            fs::read(path.join("captures").join(&manifest.capture.observation_id).join(name)).unwrap(),
            fs::read(destination.join(name)).unwrap()
        );
    }
    assert_eq!(outbox.status(20, None).await.unwrap().captures, before.captures);
    assert!(Outbox::replay(&path, &manifest.capture.observation_id, &destination).await.is_err());
}
#[tokio::test]
async fn quota_is_reserved_before_work_and_status_is_readable_while_owner_holds_lock() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("outbox");
    let limits = OutboxLimits { max_bytes: 1024 * 1024, max_captures: 1 };
    let mut outbox = Outbox::create(&path, limits).unwrap();
    assert_eq!(kind(&outbox.reserve("fixture", &request()).await.err().unwrap()), OutboxProblem::Capacity);
    assert_eq!(Outbox::inspect(&path, 10, None).await.unwrap().captures, 0);
    assert_eq!(kind(&Outbox::open(&path).err().unwrap()), OutboxProblem::Locked);
    let mut small = request();
    small.budget.page_bytes = 1024;
    small.budget.total_bytes = 1024;
    let lease = outbox.reserve("fixture", &small).await.unwrap();
    drop(lease);
    assert_eq!(Outbox::inspect(&path, 10, None).await.unwrap().unfinished, 1);
}
#[tokio::test]
async fn failed_commit_does_not_publish_batches_or_cursor_and_can_roll_forward() {
    let temp = tempfile::TempDir::new().unwrap();
    let (_path, mut outbox) = box_at(&temp);
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    let manifest = sealed(&mut lease).await;
    journal_fault(
        lease.outbox,
        "CREATE TRIGGER fail_head BEFORE INSERT ON range_heads BEGIN SELECT RAISE(ABORT,'fault'); END;",
    )
    .await;
    assert!(lease.finish().await.is_err());
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
    assert_eq!(
        outbox
            .files
            .run(|| |files| Ok(files
                .journal
                .conn
                .query_row("SELECT count(*) FROM batches", [], |row| row.get::<_, u32>(0))
                .unwrap()))
            .await
            .unwrap(),
        0
    );
    journal_fault(&outbox, "DROP TRIGGER fail_head").await;
    let report = outbox.recover().await.unwrap();
    assert_eq!(report.committed, 1);
    assert_eq!(outbox.verify(&manifest.capture.observation_id).await.unwrap().capture, manifest.capture);
    assert_eq!(outbox.recover().await.unwrap().committed, 0);
}
#[tokio::test]
async fn corruption_quarantines_the_capture_and_removes_its_cursor() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let manifest = complete(&mut outbox).await;
    let capture = outbox
        .files
        .run(|| {
            let id = manifest.capture.observation_id.clone();
            move |files| files.journal.capture(&id)
        })
        .await
        .unwrap();
    let range = artifacts::validate(&path.join("captures").join(&manifest.capture.observation_id), &capture, None)
        .unwrap()
        .range()
        .unwrap();
    assert!(outbox.captured_range(&range).await.unwrap().is_some());
    let file = path.join("captures").join(&manifest.capture.observation_id).join("runtime.ndjson");
    let mut bytes = fs::read(&file).unwrap();
    let position = bytes.windows(b"9007199254740993".len()).position(|value| value == b"9007199254740993").unwrap();
    bytes[position] = b'8';
    assert!(serde_json::from_slice::<ExportedRecord>(&bytes).is_ok());
    fs::write(&file, bytes).unwrap();
    assert_eq!(kind(&outbox.verify(&manifest.capture.observation_id).await.unwrap_err()), OutboxProblem::Integrity);
    assert!(outbox.captured_range(&range).await.unwrap().is_none());
    assert!(file.exists());
    assert_eq!(outbox.status(10, None).await.unwrap().entries[0].state, OutboxCaptureState::Quarantined);
}

#[tokio::test]
async fn a_mismatched_journal_receipt_prevents_replay_and_quarantines_on_verification() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let manifest = complete(&mut outbox).await;
    journal_fault(&outbox, "UPDATE batches SET sha='incorrect'").await;
    let destination = temp.path().join("replay");
    assert_eq!(
        kind(&Outbox::replay(&path, &manifest.capture.observation_id, &destination).await.unwrap_err()),
        OutboxProblem::Integrity
    );
    assert!(!destination.join("manifest.json").exists());
    assert_eq!(kind(&outbox.verify(&manifest.capture.observation_id).await.unwrap_err()), OutboxProblem::Integrity);
    assert_eq!(outbox.status(10, None).await.unwrap().entries[0].state, OutboxCaptureState::Quarantined);
}

#[tokio::test]
async fn status_pages_across_owner_epochs_without_repeating_entries() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let first = complete(&mut outbox).await;
    let second = complete(&mut outbox).await;
    drop(outbox);
    let mut outbox = Outbox::open(&path).unwrap();
    let third = complete(&mut outbox).await;
    let mut cursor = None;
    let mut observations = Vec::new();
    loop {
        let status = Outbox::inspect(&path, 1, cursor.as_deref()).await.unwrap();
        assert_eq!(status.captures, 3);
        observations.extend(status.entries.into_iter().map(|entry| entry.capture.observation_id));
        cursor = status.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        observations,
        [third.capture.observation_id, second.capture.observation_id, first.capture.observation_id]
    );
}

#[tokio::test]
async fn unexpected_or_linked_artifacts_cannot_be_committed() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, mut outbox) = box_at(&temp);
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    let manifest = sealed(&mut lease).await;
    fs::write(lease.directory().join("unexpected"), b"not a capture artifact").unwrap();
    assert_eq!(kind(&lease.finish().await.unwrap_err()), OutboxProblem::Integrity);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 0);
    fs::remove_file(path.join("captures").join(&manifest.capture.observation_id).join("unexpected")).unwrap();
    assert_eq!(outbox.recover().await.unwrap().committed, 1);

    #[cfg(unix)]
    {
        let directory = path.join("captures").join(&manifest.capture.observation_id);
        let file = directory.join("runtime.ndjson");
        let outside = temp.path().join("runtime.ndjson");
        fs::rename(&file, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &file).unwrap();
        assert_eq!(kind(&outbox.verify(&manifest.capture.observation_id).await.unwrap_err()), OutboxProblem::Integrity);
        assert!(outside.exists());
    }
}
#[tokio::test]
async fn missing_journal_or_unknown_capture_data_never_reinitializes_history() {
    let temp = tempfile::TempDir::new().unwrap();
    let (path, outbox) = box_at(&temp);
    drop(outbox);
    fs::remove_file(path.join("journal.sqlite3")).unwrap();
    assert!(Outbox::open(&path).is_err());
    assert!(!path.join("journal.sqlite3").exists());
    let other = temp.path().join("other");
    let mut outbox = Outbox::create(&other, OutboxLimits::default()).unwrap();
    fs::create_dir(other.join("captures").join("unknown")).unwrap();
    assert_eq!(kind(&outbox.recover().await.unwrap_err()), OutboxProblem::Integrity);
}

#[tokio::test]
async fn crash_child() {
    let Ok(phase) = std::env::var("CULPRIT_OUTBOX_CRASH_PHASE") else {
        return;
    };
    let path = PathBuf::from(std::env::var_os("CULPRIT_OUTBOX_CRASH_PATH").unwrap());
    let mut outbox = Outbox::open(&path).unwrap();
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    if phase == "reserved" {
        std::process::exit(73);
    }
    if phase == "active" {
        lease.source_started().await.unwrap();
        std::process::exit(73);
    }
    let manifest = sealed(&mut lease).await;
    if phase == "sealed" {
        std::process::exit(73);
    }
    if phase == "transaction" {
        let directory = lease.directory().to_owned();
        lease
            .outbox
            .files
            .run(|| {
                move |files| -> Result<()> {
                    let capture = files.journal.capture(&manifest.capture.observation_id).unwrap();
                    let validated = artifacts::validate(&directory, &capture, None).unwrap();
                    let _transaction = files.journal.prepare_commit(&capture, &validated).unwrap();
                    std::process::exit(73);
                }
            })
            .await
            .unwrap();
    }
    lease.finish().await.unwrap();
    std::process::exit(73);
}
#[tokio::test]
async fn process_death_at_each_commit_boundary_recovers_without_a_source_read() {
    for phase in ["reserved", "active", "sealed", "transaction", "committed"] {
        let temp = tempfile::TempDir::new().unwrap();
        let (path, outbox) = box_at(&temp);
        drop(outbox);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "outbox::tests::crash_child", "--nocapture"])
            .env("CULPRIT_OUTBOX_CRASH_PHASE", phase)
            .env("CULPRIT_OUTBOX_CRASH_PATH", &path)
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(73), "{}", String::from_utf8_lossy(&child.stderr));
        let mut outbox = Outbox::open(&path).unwrap();
        let report = outbox.recover().await.unwrap();
        let status = outbox.status(20, None).await.unwrap();
        assert_eq!(status.captures, 1);
        if phase == "active" {
            assert_eq!(report.source_debts, 1);
            assert_eq!(
                kind(&outbox.reserve("fixture", &request()).await.err().unwrap()),
                OutboxProblem::SourceUnconfirmed
            );
            let id = status.entries[0].capture.observation_id.clone();
            outbox.acknowledge_source_quiescence(&id).await.unwrap();
            assert_eq!(outbox.status(20, None).await.unwrap().source_debts, 0);
        } else if phase == "reserved" {
            assert_eq!(report.interrupted, 1);
            assert_eq!(status.ready, 0);
        } else {
            assert_eq!(status.ready, 1);
            assert_eq!(status.entries[0].capture.revision.as_ref().unwrap().owner_epoch, "1");
            let replay = Outbox::replay(&path, &status.entries[0].capture.observation_id, &temp.path().join("replay"))
                .await
                .unwrap();
            assert_eq!(replay.capture, status.entries[0].capture);
        }
        let next = complete(&mut outbox).await;
        assert_eq!(next.capture.revision.unwrap().owner_epoch, "2");
    }
}
