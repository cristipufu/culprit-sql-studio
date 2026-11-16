use super::*;
use crate::analytical::test_support::request_memory_investigation_fixture;

fn revisions() -> [DiagnosticRevision; 2] {
    let fixture = request_memory_investigation_fixture();
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(fixture.intent),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    };
    let first = DiagnosticRevision::with_investigation(
        &fixture.episode,
        fixture.episode.clone(),
        event,
        "2020-01-01T12:00:02Z",
    )
    .unwrap()
    .unwrap();
    let mut next = first.episode.clone();
    next.analysis = DiagnosticAnalysisStatus::Deferred;
    next.issue = Some(DiagnosticAnalysisIssue::BudgetExhausted);
    let second = DiagnosticRevision::between(Some(&first.episode), next, "2020-01-01T12:00:02Z").unwrap().unwrap();
    [first, second]
}

#[test]
fn derived_page_and_whole_output_limits_reject_before_appending_the_oversized_page() {
    for total_limit in [false, true] {
        let revisions = revisions();
        let parent = revisions[0].episode.first.capture_ref();
        let step_id = digest(b"derived byte ceiling");
        let source = CollectionDescription {
            target: revisions[0].episode.scope.target.clone(),
            partition: SourcePartition::Target,
            source: DIAGNOSTICS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
                parent: parent.clone(),
                controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
                step_id: step_id.clone(),
                analyzed_at: "2020-01-01T12:00:02Z".into(),
                records: 2,
                checkpoint_records: 1,
                scope_bindings: BTreeMap::from([(
                    revisions[0].episode.scope.key().unwrap(),
                    revisions[0].episode.scope.clone(),
                )]),
            }),
        };
        let capture = CaptureIdentity { observation_id: "derived-output-limit".into(), revision: None };
        let records = revisions
            .into_iter()
            .map(|revision| {
                TelemetryRecord::Operational(OperationalObservation {
                    source: DIAGNOSTICS_SOURCE.into(),
                    key: revision.id.clone(),
                    fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
                })
            })
            .collect::<Vec<_>>();
        let sizes = records
            .iter()
            .map(|record| {
                serde_json::to_vec(&ExportedRecord::new(&source, &capture, record.clone()).unwrap()).unwrap().len()
                    as u32
                    + 1
            })
            .collect::<Vec<_>>();
        let mut budget = local_budget();
        assert_eq!(budget.page_bytes, 512 * 1024);
        assert_eq!(budget.total_bytes, 16 * 1024 * 1024);
        if total_limit {
            budget.page_bytes = *sizes.iter().max().unwrap();
            budget.total_bytes = sizes.iter().sum::<u32>() - 1;
        } else {
            budget.page_bytes = sizes[0] - 1;
        }
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Diagnostics {
                parent,
                controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
                step_id,
            },
            budget,
        };
        request.validate().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut writer = ObservationFiles::create(&root.path().join("derived"), request, capture, None).unwrap();
        writer.describe(source).unwrap();
        writer.begin_page(0).unwrap();
        let rejected = if total_limit {
            assert_eq!(writer.write_page(0, &records[..1]).unwrap(), None);
            writer.begin_page(0).unwrap();
            &records[1..]
        } else {
            &records[..1]
        };
        let before = writer.manifest.files[0].rows;
        assert_eq!(writer.write_page(0, rejected).unwrap(), Some(CollectionStop::ByteLimit));
        assert_eq!(writer.manifest.files[0].rows, before);
        writer.stop(0, CollectionStop::ByteLimit);
        assert_eq!(writer.finish(0).unwrap().status, CollectionStatus::Incomplete(CollectionStop::ByteLimit));
    }
}
