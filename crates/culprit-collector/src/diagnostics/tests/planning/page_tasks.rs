use super::*;
use crate::analytical::test_support::{PageTaskHolderCase, page_task_holder_fixture};

fn population_recipe() -> RecipeDescriptor {
    RecipeDescriptor {
        id: "fixture.paired-task-population".into(),
        selector: RecipeSelector::PageWaitTasks,
        ..recipe()
    }
}

fn frame(second: u32, token: bool) -> Frame {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut source = fixture.activity_source;
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.at = clock(second);
    native.observed_from = clock(second);
    native.observed_until = clock(second + 1);
    let mut records = fixture.activity_rows;
    for (index, row) in records.iter_mut().enumerate() {
        let TelemetryRecord::Activity(row) = row else { panic!() };
        row.elapsed_ms = Some(if index == 0 { "120000" } else { "1" }.into());
        if index == 0 && !token {
            row.statement_identity = None;
        }
    }
    Frame { source, records }
}

#[tokio::test]
async fn later_token_visibility_neither_repairs_a_missing_first_token_nor_erases_a_valid_first_question() {
    for first_token in [false, true] {
        let mut harness = Harness::new().await;
        harness.policies = policies(vec![population_recipe()]);
        let original = harness.capture(frame(0, first_token)).await;
        assert_eq!(harness.work(1).await, 1);
        let first = harness
            .checkpoint()
            .await
            .episodes
            .into_iter()
            .find(|episode| {
                matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
            })
            .unwrap();
        assert_eq!(first.first.capture, original);
        assert_eq!(
            first.issue,
            Some(if first_token {
                DiagnosticAnalysisIssue::CostUnqualified
            } else {
                DiagnosticAnalysisIssue::MissingNativeIdentity
            })
        );
        harness.capture(frame(10, !first_token)).await;
        assert_eq!(harness.work(1).await, 1);
        let updated = harness.checkpoint().await.episodes.into_iter().find(|episode| episode.id == first.id).unwrap();
        assert_eq!(updated.first, first.first);
        assert_ne!(updated.latest, first.latest);
        assert_eq!(updated.analysis, first.analysis);
        assert_eq!(updated.issue, first.issue);
        assert_eq!(updated.lifecycle, DiagnosticLifecycle::Active);
        assert!(updated.recovery.is_none());
        assert_eq!(harness.calls.counts(), [2; 4]);
    }
}
