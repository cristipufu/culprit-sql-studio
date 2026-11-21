//! The existing empty OFF/NO POPULATION fixture proves native grammar/empty original capture, not a failed crawl.
use super::*;

mod runtime;
pub(super) use runtime::run as run_runtime;

pub(super) fn verify<'a>(fixture: &'a mut Fixture<'_>) -> futures_util::future::BoxFuture<'a, Result<()>> {
    async move {
        let (_captures, original) = *capture(fixture).await?;
        let SourceObservation::FullTextPopulations { snapshot, .. } = &original.source()?.observation else {
            return Err(crate::read::invalid_data());
        };
        assert_eq!(
            snapshot.records, 0,
            "the exact owned index was created empty with CHANGE_TRACKING=OFF, NO POPULATION"
        );
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Empty);
        assert!(original.records.is_empty());
        // Native empty-roster/identity/settlement evidence is separate from active/aborted/managed/cost qualification.
        Ok(())
    }
    .boxed()
}

// Both owned cases use the same real source factory, collector, sealed original, native accounting and
// source-free readback. The independent empty assertion stays in verify(); runtime presence is separate.
async fn capture(
    fixture: &mut Fixture<'_>,
) -> Result<Box<(crate::test_support::owned_captures::OwnedCaptures, crate::test_support::owned_captures::Captured)>> {
    let target = fixture.population_target();
    let mut captures = fixture.population_captures()?;
    fixture.begin_population_capture();
    let original =
        captures.capture(CollectionSelection::FullTextPopulations { target: Box::new(target.clone()) }).boxed().await?;
    fixture.population_capture_settled();
    captures.verify_retained(&original).boxed().await?;
    let source = original.source()?;
    let context = full_text_population_context(source)?;
    let SourceObservation::FullTextPopulations { target: actual, .. } = &source.observation else {
        return Err(crate::read::invalid_data());
    };
    assert_eq!(actual.as_ref(), &target);
    assert!(context.has_qualified_local_owner());
    assert_eq!(context.database_writable, Some(true));
    assert_eq!(original.manifest.source_queries, 10);
    let metrics = original.manifest.read_metrics.as_ref().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    assert!(!metrics.has_unconfirmed_work());
    assert!(
        metrics
            .templates
            .iter()
            .all(|task| task.failures == 0 && task.timeouts == 0 && task.unconfirmed_cancellations == 0)
    );
    Ok(Box::new((captures, original)))
}
