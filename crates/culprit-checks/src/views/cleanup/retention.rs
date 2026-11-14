use super::*;
use culprit_core::{CleanupHolder, CleanupMechanism, Metric, key, source_id};

pub(super) fn reference(row: &RetentionObservation) -> Result<CleanupReference> {
    Ok(CleanupReference {
        source: row.source.clone(),
        observation_key: row.key()?,
        identity: Some(row.identity.clone()),
    })
}

pub(super) fn boundaries(
    population: Option<&RetentionSnapshot>,
    sources: &[SourceOutcome],
    metadata_complete: bool,
) -> Result<Vec<CleanupBoundary>> {
    let Some(population) = population else { return Ok(vec![]) };
    let source_complete = population.sources.iter().all(|native| {
        sources
            .iter()
            .find(|source| source.source == native.source)
            .is_some_and(|source| matches!(source.state, SourceState::Collected | SourceState::Empty))
    });
    let canonical_complete = sources
        .iter()
        .filter(|source| {
            [source_id::OPEN_TRANSACTIONS, source_id::CLEANUP_SLOTS, source_id::CLEANUP_PREPARED]
                .contains(&source.source.as_str())
        })
        .all(|source| matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::NotApplicable));
    [CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]
        .into_iter()
        .map(|horizon| {
            let comparable = population
                .holders
                .iter()
                .flat_map(|row| &row.horizons)
                .filter(|native| native.protects.contains(&horizon))
                .all(|native| native.transactions_behind.is_some());
            let oldest_observed =
                population.oldest_observed(horizon).into_iter().map(reference).collect::<Result<_>>()?;
            Ok(CleanupBoundary {
                horizon,
                complete: metadata_complete
                    && source_complete
                    && canonical_complete
                    && comparable
                    && population.is_horizon_complete(horizon),
                oldest_observed,
            })
        })
        .collect()
}

pub(super) fn explain(row: &RetentionObservation) -> Result<CleanupExplanation> {
    let (mechanism, detail) = match row.holder {
        CleanupHolder::Session => (CleanupMechanism::SessionHorizon, t!("view.cleanup.retainer.session")),
        CleanupHolder::ReplicationSlot => (CleanupMechanism::ReplicationSlotHorizon, t!("view.cleanup.retainer.slot")),
        CleanupHolder::PreparedTransaction => {
            (CleanupMechanism::PreparedTransactionHorizon, t!("view.cleanup.retainer.prepared"))
        }
    };
    let mut explanation = vec![detail];
    if let Some(context) = &row.query {
        explanation.push(match context.relationship {
            RetentionQueryRelationship::ExecutingAtObservation => t!("view.cleanup.retainer.current_context"),
            RetentionQueryRelationship::PreviousStatement => t!("view.cleanup.retainer.previous_context"),
        });
    } else if row.unresolved_session_id.is_some() {
        explanation.push(t!("view.cleanup.retainer.unresolved_session"));
    }
    let mut metrics = vec![Metric::said_text(key!("view.cleanup.metric.observed"), row.observed_at.clone())];
    if let Some(age) = row.age_seconds {
        metrics.push(Metric::said_text(
            key!("view.cleanup.metric.age"),
            t!("common.duration.s", n = format::digits(age.to_string())),
        ));
    }
    for horizon in &row.horizons {
        metrics.push(
            Metric::text(t!("view.cleanup.metric.horizon", name = &horizon.native_name), horizon.native_value.clone())
                .keyed(&format!("native-horizon:{}", horizon.native_name)),
        );
        if let Some(distance) = &horizon.transactions_behind {
            metrics.push(
                Metric::text(
                    t!("view.cleanup.metric.distance", name = &horizon.native_name),
                    format::digits(distance.clone()),
                )
                .keyed(&format!("native-distance:{}", horizon.native_name)),
            );
        }
    }
    Ok(CleanupExplanation {
        reference: reference(row)?,
        mechanisms: vec![mechanism],
        title: t!("view.cleanup.retainer.title", name = &row.name),
        explanation: explanation.join(" "),
        metrics,
        investigation: t!("view.cleanup.retainer.investigate"),
        verify: t!("view.cleanup.retainer.verify"),
    })
}
