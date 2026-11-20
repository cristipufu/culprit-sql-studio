use super::*;

pub(super) fn entry(row: &OperationalObservation, point: &TimescaleContinuousAggregate) -> JobEntry {
    let mut metrics = vec![
        Metric::said_text(culprit_core::key!("view.jobs.metric.observed_at"), &point.observed_at),
        Metric::said_text(culprit_core::key!("view.jobs.metric.owner"), &point.owner),
        Metric::said_text(
            culprit_core::key!("view.jobs.continuous_aggregates.source"),
            relation(&point.source_hypertable),
        ),
        Metric::said_text(
            culprit_core::key!("view.jobs.continuous_aggregates.materialization"),
            relation(&point.materialization_hypertable),
        ),
        Metric::said_text(
            culprit_core::key!("view.jobs.continuous_aggregates.query_mode"),
            t!("view.jobs.continuous_aggregates.mode", materialized = point.materialized_only.to_string()),
        ),
        Metric::said_text(
            culprit_core::key!("view.jobs.continuous_aggregates.compression"),
            boolean(point.compression_enabled),
        ),
    ];
    if let Some(granular) = point.granular_refresh_enabled {
        metrics
            .push(Metric::said_text(culprit_core::key!("view.jobs.continuous_aggregates.granular"), boolean(granular)));
    }
    JobEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        job_id: None,
        kind: JobEntryKind::ContinuousAggregate,
        title: t!("view.jobs.continuous_aggregates.title", name = relation(&point.view)),
        explanation: t!("view.jobs.continuous_aggregates.explanation"),
        metrics,
        investigation: t!("view.jobs.continuous_aggregates.investigate"),
        verify: t!("view.jobs.continuous_aggregates.verify"),
    }
}
fn relation(label: &TimescaleRelationLabel) -> String {
    crate::support::qualified(culprit_core::Engine::Postgres, &label.schema, &label.name)
}
fn boolean(value: bool) -> String {
    t!("view.jobs.continuous_aggregates.boolean", value = value.to_string())
}
