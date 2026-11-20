use super::*;
fn configured() -> TimescaleJobsSnapshot {
    let mut read = input();
    let relation = |schema: &str, name: &str| TimescaleRelationLabel { schema: schema.into(), name: name.into() };
    let point = TimescaleContinuousAggregate {
        position: 1,
        observed_at: read.context.observed_from.clone(),
        view: relation("schema.with.dot", "hourly\" readings<script>"),
        source_hypertable: relation("public", "raw readings"),
        materialization_hypertable: relation("_timescaledb_internal", "_materialized_hypertable_2"),
        owner: "current-aggregate-owner".into(),
        materialized_only: false,
        compression_enabled: true,
        granular_refresh_enabled: None,
    };
    read.observations.push(OperationalObservation {
        source: TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleContinuousAggregate(Box::new(point)),
    });
    read.sources[2] = StateSourceCoverage::collected(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), 1);
    assert!(read.is_valid());
    read
}
#[test]
fn current_configuration_has_no_job_binding_and_does_not_attach_to_the_retained_failure() {
    let read = configured();
    let original = read.clone();
    let report = explain_native(&read, true).unwrap();
    let entry = report.entries.iter().find(|entry| entry.kind == JobEntryKind::ContinuousAggregate).unwrap();
    assert!(entry.job_id.is_none());
    assert_eq!(entry.source, TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE);
    assert_eq!(entry.observation_key, read.observations[1].key);
    assert_eq!(
        entry.title,
        t!("view.jobs.continuous_aggregates.title", name = "\"schema.with.dot\".\"hourly\"\" readings<script>\"")
    );
    assert!(entry.metrics.iter().any(|metric| metric.is("view.jobs.continuous_aggregates.source")
        && metric.text.as_deref() == Some("public.\"raw readings\"")));
    assert!(!entry.metrics.iter().any(|metric| metric.is("view.jobs.continuous_aggregates.granular")));
    let run = report.entries.iter().find(|entry| entry.kind == JobEntryKind::RetainedRun).unwrap();
    assert!(run.metrics.iter().all(|metric| !metric.is("view.jobs.continuous_aggregates.source")
        && !metric.is("view.jobs.continuous_aggregates.materialization")));
    assert!(report.limitations.contains(&t!("view.jobs.continuous_aggregates.limit")));
    assert_eq!((report.native_rows, report.observed_rows), (2, 2));
    assert_eq!(read, original);
}
#[test]
fn missing_configured_rows_remain_a_read_gap_without_an_empty_catalog_or_refresh_verdict() {
    let mut read = configured();
    read.observations.pop();
    let report = explain_native(&read, false).unwrap();
    assert_eq!((report.native_rows, report.observed_rows), (2, 1));
    assert!(report.entries.iter().all(|entry| entry.kind != JobEntryKind::ContinuousAggregate));
    assert!(
        report
            .sources
            .iter()
            .any(|source| source.source == TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE
                && source.state == SourceState::Partial)
    );
    assert!(report.limitations.contains(&t!("view.jobs.limit.read")));
    assert!(explain_native(&read, true).is_err());
}
#[test]
fn native_boolean_modes_are_localized_and_denied_catalogs_cannot_borrow_positive_points() {
    let mut read = configured();
    let OperationalFact::TimescaleContinuousAggregate(point) = &mut read.observations[1].fact else { panic!() };
    point.materialized_only = true;
    point.granular_refresh_enabled = Some(false);
    let report = explain_native(&read, true).unwrap();
    let entry = report.entries.iter().find(|entry| entry.kind == JobEntryKind::ContinuousAggregate).unwrap();
    assert!(entry.metrics.iter().any(|metric| metric.is("view.jobs.continuous_aggregates.query_mode")
        && metric.text.as_deref() == Some(t!("view.jobs.continuous_aggregates.mode", materialized = "true").as_str())));
    assert!(entry.metrics.iter().any(|metric| metric.is("view.jobs.continuous_aggregates.granular")
        && metric.text.as_deref() == Some(t!("view.jobs.continuous_aggregates.boolean", value = "false").as_str())));
    read.metadata.views[4].state = TimescaleJobViewState::PermissionDenied;
    assert!(explain_native(&read, true).is_err());
}
