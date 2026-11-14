use super::*;
mod planning;
mod recovery;
use crate::{
    analytical::test_support::{activity_evidence_cases, activity_observation},
    capture::{SourceFactory, collect},
};
use culprit_core::{Engine, ObservedWaitResource, WaitResourceKind};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader, admission::SourceGate};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

#[derive(Default)]
struct NativeCalls {
    opened: AtomicU32,
    observed: AtomicU32,
    verified: AtomicU32,
    closed: AtomicU32,
}
impl NativeCalls {
    fn counts(&self) -> [u32; 4] {
        [&self.opened, &self.observed, &self.verified, &self.closed].map(|count| count.load(Ordering::SeqCst))
    }
}

#[derive(Clone)]
struct Frame {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
}
struct Factory {
    frame: Frame,
    calls: Arc<NativeCalls>,
}
struct Reader {
    frame: Frame,
    calls: Arc<NativeCalls>,
    queries: u32,
}
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.calls.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FrozenSource::new(
            Reader { frame: self.frame.clone(), calls: self.calls.clone(), queries: 0 },
            request,
        )?))
    }
}
#[async_trait::async_trait]
impl ObservationReader for Reader {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        self.queries += 1;
        self.calls.observed.fetch_add(1, Ordering::SeqCst);
        Ok(FrozenObservation { description: self.frame.source.clone(), records: self.frame.records.clone() })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        self.queries += 1;
        self.calls.verified.fetch_add(1, Ordering::SeqCst);
        Ok(*description == self.frame.source)
    }
    fn query_count(&self) -> u32 {
        self.queries
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.calls.closed.fetch_add(1, Ordering::SeqCst);
        SourceShutdown::Quiescent
    }
}

struct Harness {
    root: tempfile::TempDir,
    path: PathBuf,
    outbox: Outbox,
    gate: SourceGate,
    calls: Arc<NativeCalls>,
    policies: DiagnosticPolicies,
}
impl Harness {
    async fn new() -> Self {
        Self::with_recovery_store(None).await
    }
    async fn with_recovery_store(store_id: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox");
        let mut outbox = match store_id {
            Some(id) => Outbox::create_remote(&path, OutboxLimits::default(), id),
            None => Outbox::create(&path, OutboxLimits::default()),
        }
        .unwrap();
        outbox.recover().await.unwrap();
        let inventory = FleetInventory {
            targets: vec![FleetTarget {
                id: "database".into(),
                connection: "fixture".into(),
                subject: CollectionSubject::Database,
                shard: "shard".into(),
                admission_group: "instance".into(),
                schedules: vec![FleetSchedule {
                    id: "baseline".into(),
                    every_seconds: MIN_ACTIVITY_POLL_SECONDS,
                    jitter_seconds: 0,
                    collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                    budget: CollectionBudget::default(),
                }],
            }],
        };
        Self {
            root,
            path,
            outbox,
            gate: SourceGate::default(),
            calls: Arc::new(NativeCalls::default()),
            policies: DiagnosticPolicies::from_inventory(&inventory, "shard").unwrap(),
        }
    }
    async fn capture(&mut self, frame: Frame) -> CaptureIdentity {
        let selection = match &frame.source.observation {
            SourceObservation::Activity(_) => CollectionSelection::Activity,
            SourceObservation::FocusedPageWait { result, .. } => {
                CollectionSelection::FocusedPageWait { focus: Box::new(result.focus.clone()) }
            }
            _ => panic!("the controller fixture captures native baselines and focused results only"),
        };
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection,
            budget: CollectionBudget {
                max_queries: 2,
                max_pages: 4,
                max_snapshot_rows: 4,
                page_rows: 1,
                page_bytes: 128 * 1024,
                total_bytes: 4 * 1024 * 1024,
                ..Default::default()
            },
        };
        let factory = Factory { frame, calls: self.calls.clone() };
        let mut lease = self.outbox.reserve("fixture", &request).await.unwrap();
        let identity = lease.identity().clone();
        let directory = lease.directory().to_owned();
        let manifest = collect(
            &self.gate,
            &factory,
            request,
            &directory,
            identity.clone(),
            &CollectionCancellation::default(),
            Some(&mut lease),
        )
        .await
        .unwrap();
        assert_eq!(manifest.status, CollectionStatus::Complete);
        assert_eq!(manifest.source_queries, 2);
        lease.finish().await.unwrap();
        assert!(!self.gate.is_closed());
        identity
    }
    async fn work(&mut self, limit: u32) -> u32 {
        let native_before = self.calls.counts();
        let processed =
            analyze_pending(&mut self.outbox, &self.policies, &CollectionCancellation::default(), limit, None)
                .await
                .unwrap();
        assert_eq!(self.calls.counts(), native_before, "offline analysis must never reopen a native source");
        let status = self.outbox.status(50, None).await.unwrap();
        assert_eq!((status.source_debts, status.unconfirmed_sources, status.unfinished), (0, 0, 0));
        processed
    }
    async fn checkpoint(&self) -> DiagnosticCheckpoint {
        let page = self.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
        assert!(page.next_cursor.is_none());
        assert_eq!(page.checkpoints.len(), 1);
        page.checkpoints.into_iter().next().unwrap()
    }
    async fn focus(&self, capture: &CaptureIdentity) -> ActivityWaitFocus {
        let input = self.outbox.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        assert_eq!(input.rows.len(), 1);
        let row = &input.rows[0];
        let TelemetryRecord::Activity(original) = &row.row.record else { panic!() };
        let native = input.source.observation.activity().unwrap();
        let focus = ActivityWaitFocus {
            parent: row.reference.clone(),
            target: input.source.target.clone(),
            source: input.source.source.clone(),
            epoch: native.epoch.clone().unwrap(),
            original_observed_at: native.at.clone(),
            original: original.as_ref().clone(),
            statement_identity: original.statement_identity.clone().unwrap(),
            resource: original.wait.as_ref().unwrap().identified_resources[0].clone(),
        };
        assert!(focus.matches_parent(&input.source, original, &row.reference));
        focus
    }
    async fn revisions(&self, parent: &CaptureIdentity) -> Vec<DiagnosticRevision> {
        let status = self.outbox.status(50, None).await.unwrap();
        let captures = status
            .entries
            .iter()
            .filter(|entry| {
                matches!(&entry.request.selection, CollectionSelection::Diagnostics { parent: input, .. }
                if input.capture == *parent)
            })
            .collect::<Vec<_>>();
        assert_eq!(captures.len(), 1, "each input has exactly one durable derived capture");
        let directory = self.root.path().join(format!("replay-{}", captures[0].capture.observation_id));
        let rows = replay(&self.path, &captures[0].capture, &directory).await;
        rows.into_iter()
            .filter_map(|row| match row.record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::DiagnosticRevision(revision),
                    ..
                }) => Some(*revision),
                TelemetryRecord::DiagnosticCheckpoint(_) => None,
                _ => panic!("derived diagnostics must not duplicate native activity payloads"),
            })
            .collect()
    }
}

fn clock(second: u32) -> String {
    format!("2020-01-01T12:00:{second:02}.000000000Z")
}
fn participant() -> ActivityObservation {
    let mut row = activity_observation(DefinitionPayload::sql(
        "SELECT payload FROM dbo.orders WHERE tenant_id=9264 AND token='original-private-value'",
        Engine::Mssql,
    ));
    row.elapsed_ms = Some("10".into());
    row.wait = Some(ActivityWait {
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1".into()),
        identified_resources: vec![ObservedWaitResource {
            kind: WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("7:1:4144").qualified("source", "native-wait-resource"),
        }],
        owners: vec![],
        resources: None,
    });
    assert!(row.is_valid());
    row
}
fn baseline(second: u32, rows: Vec<ActivityObservation>) -> Frame {
    let (mut source, _) = activity_evidence_cases().remove(0);
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.observed_from = clock(second);
    native.at = clock(second);
    native.observed_until = clock(second + 1);
    native.native_rows = rows.len() as u32;
    native.observations = rows.len() as u32;
    native.coverage = StateSourceCoverage::collected(source.source.clone(), rows.len() as u32);
    Frame { source, records: rows.into_iter().map(TelemetryRecord::from).collect() }
}
fn missing(focus: ActivityWaitFocus, second: u32) -> Frame {
    let source = "focused-native-pages".to_owned();
    Frame {
        source: CollectionDescription {
            target: focus.target.clone(),
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: clock(second),
                    observed_until: clock(second + 1),
                    records: 0,
                    sources: vec![StateSourceCoverage::observed(source, 0, false)],
                },
                result: Box::new(ActivityWaitFocusResult {
                    lineage: None,
                    focus,
                    observed_from: clock(second),
                    observed_until: clock(second + 1),
                    source: FocusSourceState::Matched,
                    request: PageWaitRequestState::Missing,
                    wait: PageWaitRelation::Unqualified,
                }),
            },
        },
        records: vec![],
    }
}
async fn replay(path: &Path, capture: &CaptureIdentity, directory: &Path) -> Vec<ExportedRecord> {
    let receipt = Outbox::replay(path, &capture.observation_id, directory).await.unwrap();
    assert_eq!(receipt.capture, *capture);
    let manifest: CollectionManifest =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    manifest
        .files
        .iter()
        .flat_map(|file| {
            std::fs::read_to_string(directory.join(&file.file))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn a_missing_focused_participant_keeps_original_evidence_and_unresolved_history_after_restart() {
    let mut harness = Harness::new().await;
    let native = harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let initial = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(initial.analysis, DiagnosticAnalysisStatus::Pending);
    let focus = harness.focus(&native).await;
    let original = harness.outbox.diagnostic_evidence(&focus.parent).await.unwrap();
    let later = harness.capture(missing(focus.clone(), 10)).await;
    assert_eq!(harness.work(1).await, 1);
    assert!(harness.checkpoint().await.episodes.is_empty());
    let revisions = harness.revisions(&later).await;
    assert_eq!(revisions.len(), 1);
    let episode = &revisions[0].episode;
    assert_eq!(episode.first, initial.first);
    assert_eq!(episode.latest, initial.latest);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert!(episode.report.is_none(), "a disappeared participant cannot produce a mapped header report");
    assert!(
        matches!(&episode.recovery, Some(DiagnosticRecoveryProof::FocusedParticipantGone { parent, .. }) if parent.as_ref() == &focus.parent)
    );
    assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
    let count = harness.outbox.status(50, None).await.unwrap().captures;
    drop(harness.outbox);
    let mut reopened = Outbox::open(&harness.path).unwrap();
    let recovery = reopened.recover().await.unwrap();
    assert_eq!((recovery.interrupted, recovery.quarantined, recovery.source_debts), (0, 0, 0));
    assert_eq!(reopened.diagnostic_evidence(&focus.parent).await.unwrap(), original);
    let rows = replay(&harness.path, &native, &harness.root.path().join("original-after-restart")).await;
    assert_eq!(rows, vec![original.1]);
    assert_eq!(
        analyze_pending(&mut reopened, &harness.policies, &CollectionCancellation::default(), 10, None).await.unwrap(),
        0
    );
    assert_eq!(reopened.status(50, None).await.unwrap().captures, count);
    assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
}

#[tokio::test]
async fn a_complete_empty_baseline_keeps_pending_history_until_the_later_focus_finishes_analysis() {
    let mut harness = Harness::new().await;
    let original = harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let focus = harness.focus(&original).await;
    harness.capture(baseline(10, vec![])).await;
    assert_eq!(harness.work(1).await, 1);
    let recovered = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(recovered.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(matches!(recovered.recovery, Some(DiagnosticRecoveryProof::CompleteScope { .. })));
    let later = harness.capture(missing(focus, 20)).await;
    assert_eq!(harness.work(1).await, 1);
    let revisions = harness.revisions(&later).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.first, recovered.first);
    assert_eq!(revisions[0].episode.recovery, recovered.recovery, "the first qualified recovery proof is immutable");
    assert_eq!(revisions[0].episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert!(harness.checkpoint().await.episodes.is_empty());
}

#[tokio::test]
async fn a_delayed_middle_parent_is_authenticated_after_a_newer_baseline_without_rewriting_originals() {
    let mut harness = Harness::new().await;
    let first = harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let first_focus = harness.focus(&first).await;
    let mut row = participant();
    row.elapsed_ms = Some("20".into());
    let middle = harness.capture(baseline(10, vec![row.clone()])).await;
    assert_eq!(harness.work(1).await, 1);
    let focus = harness.focus(&middle).await;
    row.elapsed_ms = Some("30".into());
    let latest = harness.capture(baseline(20, vec![row])).await;
    assert_eq!(harness.work(1).await, 1);
    let latest_focus = harness.focus(&latest).await;
    let before = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(before.first, first_focus.parent);
    assert_eq!(before.latest, latest_focus.parent);
    assert!(before.peak.is_none());
    assert_ne!(focus.parent, before.first);
    assert_ne!(focus.parent, before.latest);
    let later = harness.capture(missing(focus.clone(), 30)).await;
    assert_eq!(harness.work(1).await, 1);
    let revisions = harness.revisions(&later).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.first, before.first);
    assert_eq!(revisions[0].episode.latest, before.latest);
    assert_eq!(revisions[0].episode.revision, "4");
    assert!(revisions[0].inputs.contains(&focus.parent));
    assert_eq!(
        harness.outbox.diagnostic_evidence(&focus.parent).await.unwrap().1.record,
        TelemetryRecord::from(focus.original)
    );
    assert!(harness.checkpoint().await.episodes.is_empty());
}

#[tokio::test]
async fn a_valid_old_focus_is_acknowledged_without_modifying_a_new_recurrence_or_poisoning_the_queue() {
    let mut harness = Harness::new().await;
    let first = harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let old_focus = harness.focus(&first).await;
    harness.capture(missing(old_focus.clone(), 10)).await;
    assert_eq!(harness.work(1).await, 1);
    harness.capture(baseline(20, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let recurrence = harness.checkpoint().await.episodes.remove(0);
    assert_ne!(recurrence.first, old_focus.parent);
    let late = harness.capture(missing(old_focus, 30)).await;
    assert_eq!(harness.work(1).await, 1);
    assert!(harness.revisions(&late).await.is_empty());
    assert_eq!(harness.checkpoint().await.episodes, vec![recurrence]);
    assert!(harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    harness.capture(baseline(40, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1, "an unrelated valid future input must remain processable");
}

#[tokio::test]
async fn cancellation_and_turn_limits_leave_sealed_inputs_pending_without_native_reentry() {
    let mut harness = Harness::new().await;
    let first = harness.capture(baseline(0, vec![participant()])).await;
    let second = harness.capture(baseline(10, vec![participant()])).await;
    let cancelled = CollectionCancellation::default();
    cancelled.cancel();
    assert_eq!(analyze_pending(&mut harness.outbox, &harness.policies, &cancelled, 1, None).await.unwrap(), 0);
    assert_eq!(harness.outbox.status(50, None).await.unwrap().captures, 2);
    assert_eq!(
        harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap().parent.capture,
        first
    );
    assert_eq!(harness.work(1).await, 1);
    assert_eq!(
        harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap().parent.capture,
        second
    );
    assert_eq!(harness.work(1).await, 1);
    assert_eq!(harness.work(1).await, 0);
    assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
}
