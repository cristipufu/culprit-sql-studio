//! Canonical retained parents around the existing provider projection/decoder corpus.
use super::*;
use crate::activity::page_targets::{task_waits, tests::reader};

pub(super) struct RetainedScenario {
    pub(super) activity_source: CollectionDescription,
    pub(super) page_source: CollectionDescription,
    pub(super) rows: Vec<ActivityObservation>,
    pub(super) references: Vec<DiagnosticEvidenceRef>,
    pub(super) page: PageWaitObservation,
}

impl RetainedScenario {
    pub(super) async fn new() -> Self {
        // Reuse the provider's real projection/decoder fixture; no new fake reader or dependency feature.
        let task = task_waits::test_task_row("0000000000000101", 1);
        let mut native = reader(task_waits::test_replies(vec![task]));
        let captured = native.capture_page_wait_targets().await.unwrap();
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        Self::from_capture(captured)
    }

    pub(super) fn from_capture(captured: crate::activity::page_targets::PageWaitCapture) -> Self {
        let page = captured.snapshot.observations[0].clone();
        let activity_source = CollectionDescription {
            target: captured.collection_target(),
            partition: captured.source_partition().unwrap(),
            source: culprit_core::source_id::SESSIONS.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: captured.context.epoch.clone(),
                database_writable: captured.database_writable(),
                observed_from: page.observed_from.clone(),
                at: page.observed_from.clone(),
                observed_until: page.observed_from.clone(),
                scope: culprit_core::MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: 1,
                observations: 1,
                coverage: StateSourceCoverage::collected(culprit_core::source_id::SESSIONS.into(), 1),
            }),
        };
        let page_source = CollectionDescription {
            target: activity_source.target.clone(),
            partition: activity_source.partition.clone(),
            source: page.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitTargets {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: page.observed_from.clone(),
                    observed_until: page.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
                },
                database_writable: captured.database_writable(),
                original_activity: None,
            },
        };
        let mut scenario =
            Self { activity_source, page_source, rows: vec![page.request.clone()], references: Vec::new(), page };
        scenario.rebind_originals();
        scenario
    }

    pub(super) fn rebind_originals(&mut self) {
        let SourceObservation::Activity(native) = &mut self.activity_source.observation else { panic!() };
        native.native_rows = self.rows.len() as u32;
        native.observations = self.rows.len() as u32;
        native.coverage = StateSourceCoverage::collected(self.activity_source.source.clone(), native.observations);
        let capture = CaptureIdentity {
            observation_id: "controlled-identity-witness-original".into(),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-identity-witness".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        self.references = self
            .rows
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let exported = ExportedRecord::new(&self.activity_source, &capture, row.clone().into()).unwrap();
                DiagnosticEvidenceRef::from_export(
                    &self.activity_source,
                    &digest(b"controlled identity-witness original manifest"),
                    &exported,
                    ordinal as u32,
                )
                .unwrap()
            })
            .collect();
    }

    pub(super) fn frame(&self, receipts: bool) -> ActivityFrameEvidence<'_> {
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(ordinal, observation)| ActivityFrameRow {
                observation,
                reference: receipts.then(|| &self.references[ordinal]),
            })
            .collect::<Vec<_>>();
        ActivityFrameEvidence::new(&self.activity_source, &rows, true).unwrap()
    }

    pub(super) fn plan(&self, receipts: bool) -> DiagnosticFocusPlan {
        let capture = CaptureIdentity {
            observation_id: "controlled-identity-witness-page".into(),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-identity-witness".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        };
        let row = OperationalObservation {
            source: self.page.source.clone(),
            key: self.page.key().unwrap(),
            fact: OperationalFact::PageWait(Box::new(self.page.clone())),
        };
        let exported = ExportedRecord::new(&self.page_source, &capture, TelemetryRecord::Operational(row)).unwrap();
        let reference = DiagnosticEvidenceRef::from_export(
            &self.page_source,
            &digest(b"controlled identity-witness page manifest"),
            &exported,
            0,
        )
        .unwrap();
        plan_page_task_holder_focus(&self.page_source, &self.page, &reference, &self.frame(receipts)).unwrap()
    }
}
