use super::*;
use crate::analytical::test_support::{PageWriterCase, investigation_fixture, page_writer_fixture};

async fn mapped_input(kind: &str) -> (Harness, DiagnosticInput) {
    let fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
    let mut activity_source = fixture.activity_source;
    let mut originals = fixture.activity_rows;
    originals.truncate(1);
    let TelemetryRecord::Activity(original) = &mut originals[0] else { panic!() };
    if kind == "io" {
        let wait = original.wait.as_mut().unwrap();
        wait.native_type = Some("PAGEIOLATCH_SH".into());
        wait.identified_resources[0].kind = WaitResourceKind::PageIo;
    }
    let SourceObservation::Activity(native) = &mut activity_source.observation else { panic!() };
    native.native_rows = 1;
    native.observations = 1;
    native.coverage = StateSourceCoverage::collected(activity_source.source.clone(), 1);
    let mut harness = Harness::new().await;
    let native = harness.capture(Frame { source: activity_source, records: originals }).await;
    assert_eq!(harness.work(1).await, 1);
    let original =
        harness.outbox.diagnostic_input(&native.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    let TelemetryRecord::Activity(row) = &original.rows[0].row.record else { panic!() };
    let checkpoint = harness.checkpoint().await;
    let episode =
        checkpoint.episodes.iter().find(|episode| matches!(episode.signal, DiagnosticSignal::PageWait { .. })).unwrap();
    let PageWaitFocusPlan::Ready(focus) =
        episode.plan_page_wait_focus(&original.source, row, &original.rows[0].reference).unwrap()
    else {
        panic!()
    };
    let mut source = fixture.page_source;
    let mut records = fixture.page_rows;
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::PageWait(page) = &mut row.fact else { panic!() };
    page.request = focus.participant.original.clone();
    page.resource = focus.resource.clone();
    if kind == "outside" {
        page.resolution = PageWaitResolution::OutsideDatabase;
        page.target = None;
        page.statement_identity = None;
        page.plan_identity = None;
    } else if kind == "allocation" {
        let target = page.target.as_mut().unwrap();
        target.kind = PageWaitTargetKind::Pfs;
        target.header.page_type = Some(11);
        target.header.object_id = None;
        target.header.index_id = None;
        target.header.partition_id = None;
        target.object = None;
    }
    let mut snapshot = source.observation.snapshot().unwrap().clone();
    // The generic corpus snapshot encloses a broader page-read window. A focused description owns the exact
    // question bounds, so re-observe those header fields together rather than mixing the two descriptions.
    snapshot.observed_from = page.observed_from.clone();
    snapshot.observed_until = page.observed_until.clone();
    source.observation = SourceObservation::FocusedPageWait {
        snapshot,
        result: Box::new(ActivityWaitFocusResult {
            focus: *focus,
            observed_from: page.observed_from.clone(),
            observed_until: page.observed_until.clone(),
            source: FocusSourceState::Matched,
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
    };
    row.key = page.key().unwrap();
    let SourceObservation::FocusedPageWait { result, .. } = &source.observation else { panic!() };
    assert!(
        source.matches_request(&CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::FocusedPageWait { focus: Box::new(result.focus.clone()) },
            budget: CollectionBudget::default(),
        }),
        "the focused fixture must satisfy its real descriptor/request boundary before capture"
    );
    assert!(result.accepts_observation(page), "the independently retained page must match the original request");
    let capture = harness.capture(Frame { source, records }).await;
    let input = harness.outbox.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    (harness, input)
}

fn writer_recipe(input: &DiagnosticInput, measured: bool) -> RecipeDescriptor {
    if !measured {
        return RecipeDescriptor {
            id: "fixture.page-writers".into(),
            selector: RecipeSelector::FocusedPageWaitWriters,
            ..recipe()
        };
    }
    let mut recipe = investigation_fixture().recipe;
    recipe.id = "fixture.page-writers".into();
    recipe.selector = RecipeSelector::FocusedPageWaitWriters;
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.recipe_id = recipe.id.clone();
    receipt.selector = recipe.selector;
    receipt.scope = input
        .checkpoint
        .as_ref()
        .unwrap()
        .episodes
        .iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::PageWait { .. }))
        .unwrap()
        .scope
        .clone()
        .into();
    recipe
}

#[tokio::test]
async fn writer_eligibility_uses_the_same_native_page_contract_before_explaining_unqualified_metadata() {
    let (harness, input) = mapped_input("rowstore").await;
    for (catalog, expected) in [
        (vec![writer_recipe(&input, false)], DiagnosticAnalysisIssue::CostUnqualified),
        (vec![], DiagnosticAnalysisIssue::Unsupported),
        (vec![recipe()], DiagnosticAnalysisIssue::Unsupported),
        (vec![writer_recipe(&input, true)], DiagnosticAnalysisIssue::EvidenceUnavailable),
    ] {
        let policy = policies(catalog);
        let mut update = analyze(&input, &ActivityPolicy::default(), &clock(3)).unwrap();
        let native_calls = harness.calls.counts();
        crate::diagnostics::planning::qualify_questions(&input, policy.assignment(&input).unwrap(), &mut update)
            .unwrap();
        let episode = update
            .checkpoint
            .episodes
            .iter()
            .find(|episode| matches!(episode.signal, DiagnosticSignal::PageWait { .. }))
            .unwrap();
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
        assert_eq!(episode.issue.as_ref(), Some(&expected));
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
        assert!(matches!(episode.report, Some(DiagnosticReport::PageWait { .. })));
        assert!(episode.recovery.is_none());
        assert_eq!(harness.calls.counts(), native_calls, "declarations never construct a native source");
    }
}

#[tokio::test]
async fn outside_database_io_and_allocation_results_do_not_acquire_writer_stage_metadata() {
    for kind in ["outside", "io", "allocation"] {
        let (harness, input) = mapped_input(kind).await;
        let policy = policies(vec![writer_recipe(&input, false)]);
        let mut update = analyze(&input, &ActivityPolicy::default(), &clock(3)).unwrap();
        let expected = update.clone();
        let native_calls = harness.calls.counts();
        crate::diagnostics::planning::qualify_questions(&input, policy.assignment(&input).unwrap(), &mut update)
            .unwrap();
        assert_eq!(update.step_id, expected.step_id, "{kind}");
        assert_eq!(update.checkpoint, expected.checkpoint, "{kind}");
        assert_eq!(update.revisions, expected.revisions, "{kind}");
        assert_eq!(harness.calls.counts(), native_calls, "{kind}");
    }
}
