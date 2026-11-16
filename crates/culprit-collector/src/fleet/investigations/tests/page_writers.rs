//! Sealed native fixture facts exercise two-stage ownership; no receipt here qualifies production query cost.
use super::{
    runtime::Runtime,
    support::{Frame, Harness, seal},
};
use crate::{
    analytical::test_support::{PageWriterCase, investigation_fixture, page_writer_fixture},
    capture::CollectionCancellation,
    diagnostics::drain_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
};
use culprit_core::telemetry::*;

pub(in crate::fleet::investigations) fn mapped_frame(focus: &ActivityWaitFocus) -> Frame {
    let fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
    let mut source = fixture.page_source;
    let mut records = fixture.page_rows;
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::PageWait(page) = &mut row.fact else { panic!() };
    move_page(page, focus, 1);
    row.key = page.key().unwrap();
    let mut snapshot = source.observation.snapshot().unwrap().clone();
    snapshot.observed_from = page.observed_from.clone();
    snapshot.observed_until = page.observed_until.clone();
    source.target = focus.participant.target.clone();
    source.partition = focus.participant.partition.clone();
    source.observation = SourceObservation::FocusedPageWait {
        snapshot,
        result: Box::new(ActivityWaitFocusResult {
            focus: focus.clone(),
            observed_from: page.observed_from.clone(),
            observed_until: page.observed_until.clone(),
            source: FocusSourceState::Matched,
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
    };
    Frame { source, records }
}

pub(in crate::fleet::investigations) fn focused_frame(focus: &PageWaitWriterFocus, original: &Frame) -> Frame {
    let fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
    let mut source = fixture.source;
    let mut records = fixture.records;
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::PageWaitWriters(point) = &mut row.fact else { panic!() };
    let native = point.candidates.clone();
    point.focus_key = focus.key().unwrap();
    let page = point.page.as_mut().unwrap();
    move_page(page, &focus.waiter, 3);
    page.target = Some(focus.target.clone());
    point.observed_from = page.observed_from.clone();
    point.observed_until = page.observed_until.clone();
    point.candidates = focus
        .candidates
        .iter()
        .map(|selected| {
            let request = original
                .records
                .iter()
                .find_map(|record| match record {
                    TelemetryRecord::Activity(row) if row.key == selected.key => Some(row.as_ref()),
                    _ => None,
                })
                .unwrap();
            let Some(mut candidate) = native
                .iter()
                .find(|candidate| candidate.request.as_ref().is_some_and(|row| row.key == selected.key))
                .cloned()
            else {
                return PageWaitWriterCandidate {
                    parent: selected.parent.clone(),
                    request_state: PageWaitRequestState::SameStatement,
                    request: Some(request.clone()),
                    plan_identity: None,
                    plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                    relation: PageWriterRelation::Unavailable,
                    binding: None,
                };
            };
            candidate.parent = selected.parent.clone();
            if let Some(proof) = candidate.binding.take() {
                candidate.binding = Some(
                    MssqlPageWritePlanProof::new(
                        &focus.target,
                        &candidate,
                        proof.native_plan_sha256,
                        proof.native_statement_sha256,
                        proof.accesses,
                    )
                    .unwrap(),
                );
            }
            candidate
        })
        .collect();
    let SourceObservation::PageWaitWriters { snapshot, focus: declared, database_writable } = &mut source.observation
    else {
        panic!()
    };
    **declared = focus.clone();
    *database_writable = focus.waiter.participant.database_writable;
    snapshot.observed_from = point.observed_from.clone();
    snapshot.observed_until = point.observed_until.clone();
    snapshot.sources[0] = StateSourceCoverage::observed(source.source.clone(), 1, false);
    source.target = focus.waiter.participant.target.clone();
    source.partition = focus.waiter.participant.partition.clone();
    row.key = point.key().unwrap();
    Frame { source, records }
}

fn move_page(page: &mut PageWaitObservation, focus: &ActivityWaitFocus, seconds: i64) {
    let from = chrono::DateTime::parse_from_rfc3339(&focus.participant.original_observed_at).unwrap()
        + chrono::Duration::seconds(seconds);
    page.observed_from = from.to_rfc3339();
    page.observed_until = (from + chrono::Duration::milliseconds(500)).to_rfc3339();
    page.request = focus.participant.original.clone();
    page.resource = focus.resource.clone();
    page.statement_identity = Some(focus.participant.statement_identity.clone());
    page.before_check.as_mut().unwrap().observed_at = page.observed_from.clone();
    page.after_check.as_mut().unwrap().observed_at = page.observed_until.clone();
}

pub(in crate::fleet::investigations) async fn fixture() -> Harness {
    let fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
    let mut harness =
        Harness::with_activity(Frame { source: fixture.activity_source, records: fixture.activity_rows }).await;
    harness.backend.focus_request = PageWaitRequestState::SameStatement;
    harness.backend.focus_resolution = Some(PageWaitResolution::ObservedAtBothBounds);
    let mut writer = harness.backend.recipe.clone();
    writer.id = "fixture.page-wait-writers".into();
    writer.selector = RecipeSelector::FocusedPageWaitWriters;
    let RecipeCostQualification::Measured(receipt) = &mut writer.cost else { panic!() };
    receipt.recipe_id = writer.id.clone();
    receipt.selector = writer.selector;
    // A different authenticated experiment receipt makes selection of the second-stage authority observable.
    let cost = investigation_fixture();
    let mut source = cost.source;
    source.target = harness.backend.activity.source.target.clone();
    receipt.evidence = seal(&mut harness.outbox, Frame { source, records: vec![cost.record.record] }).await;
    harness.backend.additional_recipes.push(writer);
    harness
}

async fn mapped() -> Runtime {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    let mapped = revisions
        .iter()
        .find(|revision| matches!(revision.episode.report, Some(DiagnosticReport::PageWait { .. })))
        .unwrap();
    assert!(mapped.episode.analysis.needs_work());
    assert_eq!(mapped.episode.issue, Some(DiagnosticAnalysisIssue::EvidenceUnavailable));
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    runtime
}

pub(in crate::fleet::investigations) async fn mapped_harness() -> Harness {
    mapped().await.fixture
}

#[tokio::test]
async fn a_completed_mapping_dispatches_a_distinct_writer_question_with_original_sealed_roster_and_its_own_cost() {
    let mut runtime = mapped().await;
    assert_eq!(runtime.native_calls(), [2; 5]);
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!() };
    let intent = intent.clone();
    let DiagnosticFocus::PageWaitWriters { focus } = &intent.focus else { panic!() };
    assert_eq!(focus.candidates.len(), 3, "waiter and both original MARS contexts retain separate exact locators");
    let recipe = &runtime.fixture.backend.additional_recipes[0];
    let RecipeCostQualification::Measured(cost) = &recipe.cost else { panic!() };
    assert_eq!(intent.qualification.evidence, cost.evidence);
    assert_ne!(intent.qualification.selector, runtime.fixture.backend.recipe.selector);
    for reference in intent.focus.references() {
        runtime.fixture.outbox.diagnostic_evidence(reference).await.unwrap();
    }
    let job = runtime.take_focus().await;
    assert_eq!(job.cooldown_seconds, 60);
    assert!(job.matches_intent(&intent));
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    let writer = revisions.iter().find(|revision| revision.episode.id == intent.episode_id).unwrap();
    let Some(DiagnosticReport::PageWaitWriters { page, candidates, blocking_contexts, .. }) = &writer.episode.report
    else {
        panic!()
    };
    assert_eq!(page.as_ref(), &focus.page);
    assert_eq!(candidates, &focus.candidates.iter().map(|candidate| candidate.parent.clone()).collect::<Vec<_>>());
    assert_eq!(blocking_contexts.len(), 2);
    assert!(writer.episode.report.as_ref().unwrap().references().all(|reference| writer.inputs.contains(reference)));
    assert_eq!(writer.episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(writer.episode.recovery.is_none());
    assert_eq!(writer.episode.latest, focus.waiter.participant.parent);
    assert_eq!(runtime.native_calls(), [3; 5], "the second stage adds one bounded source, never another baseline");
    runtime.reconcile().await;
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    assert!(!runtime.enroll().await, "an incomplete bounded roster does not spin another completed writer stage");
    assert_eq!(runtime.native_calls(), [3; 5]);
}

#[tokio::test]
async fn a_shared_page_question_keeps_its_native_limit_instead_of_acquiring_unqualified_writer_metadata() {
    let mut fixture = Harness::shared().await;
    fixture.backend.focus_request = PageWaitRequestState::SameStatement;
    fixture.backend.focus_resolution = Some(PageWaitResolution::OutsideDatabase);
    fixture.backend.additional_recipes.push(RecipeDescriptor {
        id: "fixture.shared-page-writers-unavailable".into(),
        version: 1,
        purpose: RecipePurpose::Investigation,
        selector: RecipeSelector::FocusedPageWaitWriters,
        subjects: vec![RecipeSubject::Shared(SharedResourceKind::Instance)],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Unqualified,
    });
    let mut runtime = Runtime::from_fixture(fixture).await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    assert!(matches!(job.subject, CollectionSubject::Shared { .. }));
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let before = runtime.native_calls();
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    let episode = &revisions
        .iter()
        .find(|revision| matches!(revision.episode.report, Some(DiagnosticReport::PageWait { .. })))
        .unwrap()
        .episode;
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::Unsupported));
    assert_eq!(runtime.native_calls(), before);
    assert!(episode.recovery.is_none());
}
