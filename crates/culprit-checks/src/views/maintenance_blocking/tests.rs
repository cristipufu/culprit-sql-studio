use super::*;
use crate::views::focused_blocking::tests::evidence;
use culprit_core::Engine;

struct Pair {
    source: CollectionDescription,
    snapshot: MaintenanceProgressSnapshot,
    receipt: DiagnosticEvidenceRef,
    later: CollectionDescription,
    rows: Vec<ActivityObservation>,
    references: Vec<DiagnosticEvidenceRef>,
}

fn capture(name: &str, sequence: &str) -> CaptureIdentity {
    CaptureIdentity {
        observation_id: name.into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"controlled maintenance/blocking test outbox"),
            owner_epoch: "1".into(),
            sequence: sequence.into(),
        }),
    }
}

impl Pair {
    fn new(qualified: bool) -> Self {
        let (later, mut rows) = evidence(qualified);
        let SourceObservation::FocusedBlocking { snapshot: native, result } = &later.observation else { panic!() };
        rows[1].key.session.id = "44".into();
        rows[1].key.session.generation = "independently-observed-later-backend".into();
        let generation = rows[1].key.session.generation.clone();
        rows[0].wait.as_mut().unwrap().owners =
            vec![ActivityWaitOwner::Session { id: "44".into(), generation: Some(generation) }];
        let point = MaintenanceProgressObservation {
            observed_at: "2020-01-01T12:00:01Z".into(),
            actor: result.focus.original.key.clone(),
            statement: Some(result.focus.statement_identity.clone()),
            database_oid: "16384".into(),
            relation_oid: "16420".into(),
            index_oid: Some("16421".into()),
            command: MaintenanceCommand::CreateIndexConcurrently,
            phase: "waiting for writers before build".into(),
            work: MaintenanceProgressWork::PostgreSqlIndexBuild {
                progress: Box::new(PostgresIndexBuildProgress {
                    lockers_total: Some("9007199254740993".into()),
                    lockers_done: Some("0".into()),
                    current_locker_pid: Some("43".into()),
                    ..Default::default()
                }),
            },
        };
        let source = CollectionDescription {
            target: later.target.clone(),
            partition: later.partition.clone(),
            source: "postgres-maintenance-progress".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::MaintenanceProgress {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::MaintenanceProgress,
                    observed_from: point.observed_at.clone(),
                    observed_until: point.observed_at.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed("pg_stat_progress_create_index".into(), 1, false)],
                },
                result: Box::new(FocusedRequestResult {
                    focus: Some(Box::new(result.focus.clone())),
                    database_writable: native.database_writable,
                    binding: result.binding,
                    membership: FocusedRequestMembership::Observed,
                }),
            },
        };
        let SourceObservation::MaintenanceProgress { snapshot: native, result } = &source.observation else { panic!() };
        let snapshot = MaintenanceProgressSnapshot {
            source: native.sources[0].source.clone(),
            context: result.read_context(&source, native),
            coverage: native.sources[0].clone(),
            result: result.as_ref().clone(),
            observations: vec![point.clone()],
        };
        let exported = ExportedRecord::new(
            &source,
            &capture("earlier-maintenance-read", "2"),
            TelemetryRecord::Operational(OperationalObservation {
                source: snapshot.source.clone(),
                key: point.key().unwrap(),
                fact: OperationalFact::MaintenanceProgress(Box::new(point)),
            }),
        )
        .unwrap();
        let receipt =
            DiagnosticEvidenceRef::from_export(&source, &digest(b"earlier native progress"), &exported, 0).unwrap();
        let references = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let exported =
                    ExportedRecord::new(&later, &capture("later-blocking-read", "3"), row.clone().into()).unwrap();
                DiagnosticEvidenceRef::from_export(&later, &digest(b"later blocking closure"), &exported, index as u32)
                    .unwrap()
            })
            .collect();
        Self { source, snapshot, receipt, later, rows, references }
    }

    fn progress(&self, complete: bool) -> MaintenanceProgressEvidence<'_> {
        MaintenanceProgressEvidence {
            source: &self.source,
            snapshot: &self.snapshot,
            observation: (!self.snapshot.observations.is_empty()).then_some(&self.receipt),
            read_complete: complete,
        }
    }

    fn frame(&self, indices: &[usize], complete: bool) -> ActivityFrameEvidence<'_> {
        let rows = indices
            .iter()
            .map(|index| ActivityFrameRow {
                observation: &self.rows[*index],
                reference: Some(&self.references[*index]),
            })
            .collect::<Vec<_>>();
        ActivityFrameEvidence::new(&self.later, &rows, complete).unwrap()
    }
}

#[test]
fn canonical_explanations_keep_distinct_native_receipts_and_do_not_assign_the_later_owner_to_the_old_phase() {
    let pair = Pair::new(true);
    let frame = pair.frame(&[0, 1], true);
    let report = explain(pair.progress(true), &frame).unwrap();
    assert_eq!(report.maintenance_observation.as_ref(), Some(&pair.receipt));
    assert_eq!(report.original, report.blocking.original);
    assert_ne!(report.maintenance_observation.as_ref().unwrap().capture, report.original.capture);
    assert_eq!(report.maintenance.entries[0].wait.as_ref().unwrap().unresolved_locker_pid.as_deref(), Some("43"));
    assert_eq!(report.blocking.blocking.as_ref().unwrap().chains[0].tree.session, "44");
    assert_eq!(
        serde_json::to_value(report.maintenance).unwrap(),
        serde_json::to_value(super::super::maintenance_progress::explain(&pair.snapshot, true).unwrap()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(report.blocking).unwrap(),
        serde_json::to_value(
            super::super::focused_blocking::explain(frame.source(), &frame.rows().collect::<Vec<_>>(), true).unwrap(),
        )
        .unwrap()
    );
}

#[test]
fn missing_either_body_keeps_the_other_read_and_original_membership_without_a_completion_verdict() {
    let mut pair = Pair::new(true);
    pair.snapshot.observations.clear();
    let report = explain(pair.progress(false), &pair.frame(&[0, 1], true)).unwrap();
    assert!(report.maintenance_observation.is_none());
    assert_eq!(report.maintenance.native_rows, 1);
    assert_eq!(report.maintenance.observed_rows, 0);
    assert_eq!(report.maintenance.membership, FocusedRequestMembership::Observed);
    assert!(!report.maintenance.read_complete && report.blocking.read_complete);
    assert!(report.blocking.blocking.is_some());

    let pair = Pair::new(true);
    let report = explain(pair.progress(true), &pair.frame(&[1], false)).unwrap();
    assert!(report.maintenance.read_complete && !report.blocking.read_complete);
    assert_eq!(report.blocking.native_rows, 2);
    assert_eq!(report.blocking.observed_rows, 1);
    assert!(report.blocking.blocking.is_none());
}

#[test]
fn current_member_proof_cannot_upgrade_the_original_target_scope() {
    let pair = Pair::new(false);
    let report = explain(pair.progress(true), &pair.frame(&[0, 1], true)).unwrap();
    assert_eq!(report.maintenance.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert_eq!(report.blocking.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert!(report.limitations.contains(&t!("view.blocking.limit.original_scope")));
}

#[test]
fn an_altered_progress_payload_or_receipt_and_a_blocking_frame_without_receipts_are_rejected() {
    let mut pair = Pair::new(true);
    pair.receipt.payload_sha256 = digest(b"other native body");
    assert!(explain(pair.progress(true), &pair.frame(&[0, 1], true)).is_err());
    let mut pair = Pair::new(true);
    pair.snapshot.observations[0].phase = "waiting for old snapshots".into();
    assert!(explain(pair.progress(true), &pair.frame(&[0, 1], true)).is_err());
    let pair = Pair::new(true);
    let rows =
        pair.rows.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(&pair.later, &rows, true).unwrap();
    assert!(explain(pair.progress(true), &frame).is_err());
    assert_eq!(pair.source.target.engine(), Engine::Postgres);
}
