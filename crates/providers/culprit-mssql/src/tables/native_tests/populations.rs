//! The existing empty OFF/NO POPULATION fixture proves native grammar/empty original capture, not a failed crawl.
use super::*;
pub(super) fn verify<'a>(fixture: &'a mut Fixture<'_>) -> futures_util::future::BoxFuture<'a, Result<()>> {
    async move {
        let target = fixture.population_target();
        let mut captures = fixture.population_captures()?;
        // Same existing source factory, collector, retained native owner and sealed Outbox path.
        fixture.begin_population_capture();
        let original = captures
            .capture(CollectionSelection::FullTextPopulations { target: Box::new(target.clone()) })
            .boxed()
            .await?;
        // capture() returns only after the existing collector has settled the source and sealed the original.
        fixture.population_capture_settled();
        captures.verify_retained(&original).boxed().await?;
        let source = original.source()?;
        let context = full_text_population_context(source)?;
        let SourceObservation::FullTextPopulations { snapshot, target: actual, .. } = &source.observation else {
            return Err(crate::read::invalid_data());
        };
        assert_eq!(actual.as_ref(), &target);
        assert!(context.has_qualified_local_owner());
        assert_eq!(context.database_writable, Some(true));
        assert_eq!(
            snapshot.records, 0,
            "the exact owned index was created empty with CHANGE_TRACKING=OFF, NO POPULATION"
        );
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Empty);
        assert!(original.records.is_empty());
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
        // Native empty-roster/identity/settlement evidence is separate from active/aborted/managed/cost qualification.
        Ok(())
    }
    .boxed()
}
