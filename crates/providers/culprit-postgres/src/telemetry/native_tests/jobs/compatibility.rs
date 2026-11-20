use super::support::*;
use crate::read::{NativeRead, QueryRead};
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
#[ignore = "requires nonce-owned Timescale 2.31 fixture prepared by dev/timescale/jobs.sql; see dev/timescale/jobs.md"]
async fn installed_public_job_views_retain_paused_states_actual_failures_and_exact_native_counts() {
    let fixture = Fixture::open().await;
    let captured = capture(&fixture.admin, request()).await;
    assert_eq!(
        captured.manifest.source_queries, 8,
        "setup, two metadata reads, two discoveries, states, history and aggregate configuration"
    );
    let read = &captured.snapshot;
    assert_eq!(read.metadata.extension.as_ref().unwrap().qualifiers.get("version").map(String::as_str), Some("2.31.0"));
    assert_eq!(read.metadata.run_view, Some(TimescaleJobViewKind::History));
    assert_eq!(read.metadata.run_visibility, TimescaleJobRunVisibility::DatabaseOwner);
    for kind in [
        TimescaleJobViewKind::Jobs,
        TimescaleJobViewKind::Statistics,
        TimescaleJobViewKind::History,
        TimescaleJobViewKind::Errors,
    ] {
        let view = read.metadata.view(kind).unwrap();
        assert_eq!(view.state, TimescaleJobViewState::Available);
        assert!(view.identity.as_ref().unwrap().qualifiers.contains_key("columns"));
    }
    let aggregates = read
        .observations
        .iter()
        .filter_map(|row| match &row.fact {
            OperationalFact::TimescaleContinuousAggregate(point) => Some(point.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(coverage(read, TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE).state, SourceState::Collected);
    let aggregate = aggregates
        .iter()
        .find(|point| point.view.schema == "public" && point.view.name == "sensor_hourly")
        .expect("the existing owned WITH NO DATA aggregate");
    assert_eq!(aggregate.source_hypertable.schema, "public");
    assert_eq!(aggregate.source_hypertable.name, "sensor_readings");
    assert_eq!(aggregate.materialization_hypertable.schema, "_timescaledb_internal");
    assert!(!aggregate.materialization_hypertable.name.is_empty());
    assert_eq!(aggregate.owner, fixture.admin.user.as_deref().unwrap());
    assert!(!aggregate.compression_enabled);
    assert!(read.metadata.available(TimescaleJobViewKind::ContinuousAggregates));
    // The existing fixture deliberately never refreshes this view. Settings must not imply materialized data.
    for id in fixture.jobs.values() {
        let point = state(read, id);
        assert!(!point.scheduled);
        assert!(point.hypertable.is_none(), "a custom job has no inferred associated table");
        let statistics = point.statistics.as_ref().unwrap();
        assert_eq!(statistics.reported_job_state, Some(TimescaleReportedJobState::Paused));
        assert_eq!(statistics.last_run_status, Some(TimescaleJobLastRun::Failed));
        assert_eq!(statistics.total_runs.as_deref(), Some("1"));
        assert_eq!(statistics.total_failures.as_deref(), Some("1"));
        assert_eq!(statistics.total_successes.as_deref(), Some("0"));
        let runs = runs(read, id);
        assert_eq!(runs.len(), 1);
        let run = runs[0];
        assert!(run.native_run_id.is_some());
        assert_eq!(run.succeeded, Some(false));
        assert!(run.finished_at.is_some());
        assert_eq!(run.sql_state.as_deref(), Some("22012"));
        assert_eq!(run.procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::StoredOrCurrent);
    }
    assert_runs_are_partial(read);
    let json = serde_json::to_string(read).unwrap();
    assert!(!json.contains(PRIVATE_ERROR));
    assert!(!json.contains(PRIVATE_CONFIG));
    let recipe = crate::telemetry_recipes().into_iter().find(|recipe| recipe.id == "postgres.timescale-jobs").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified, "native conformance is not cost qualification");
}

#[tokio::test]
#[ignore = "requires nonce-owned Timescale jobs fixture plus smoke_reader; see dev/timescale/jobs.md"]
async fn limited_history_permission_uses_the_real_owner_filtered_error_view_without_inventing_run_verdicts() {
    let fixture = Fixture::open().await;
    let captured = capture(&fixture.limited, request()).await;
    let read = &captured.snapshot;
    assert_eq!(captured.manifest.source_queries, 8);
    assert_eq!(
        read.metadata.view(TimescaleJobViewKind::History).unwrap().state,
        TimescaleJobViewState::PermissionDenied
    );
    assert_eq!(read.metadata.run_view, Some(TimescaleJobViewKind::Errors));
    assert_eq!(read.metadata.run_visibility, TimescaleJobRunVisibility::JobOwners);
    let own = &fixture.jobs["limited"];
    assert_eq!(state(read, own).owner, fixture.limited.user.as_deref().unwrap());
    let visible = runs(read, own);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].sql_state.as_deref(), Some("22012"));
    assert!(visible[0].finished_at.is_some());
    assert!(visible[0].native_run_id.is_none());
    assert!(visible[0].succeeded.is_none(), "job_errors membership is not a native succeeded=false field");
    assert!(visible[0].failure_kind().is_none());
    assert_eq!(visible[0].procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::Stored);
    assert!(
        runs(read, &fixture.jobs["admin"]).is_empty(),
        "read permission does not grant another job owner's history"
    );
    assert_runs_are_partial(read);
    assert!(coverage(read, TIMESCALE_JOB_RUNS_SOURCE).error_code.is_none());
}

#[tokio::test]
#[ignore = "requires the nonce-owned paused configuration-check job from dev/timescale/jobs.sql"]
async fn configured_validation_function_is_recorded_without_invocation_during_collection() {
    const COUNTER: &str = "SELECT fixture.job_id::text AS job_id,calls.last_value::text AS invocations,calls.is_called FROM culprit_jobs_test.configuration_check_job fixture CROSS JOIN culprit_jobs_test.configuration_check_calls calls LIMIT 2";
    let (fixture, before, captured, after, admin_queries) = Fixture::with_admin(|fixture, reader, cancellation| {
        Box::pin(async move {
            let before = reader.query(COUNTER, 2).await.expect("owned checker invocation baseline");
            let captured = capture_with_cancellation(&fixture.admin, request(), cancellation).await;
            let after = reader.query(COUNTER, 2).await.expect("owned checker invocation final point");
            (fixture, before, captured, after, reader.queries())
        })
    })
    .await;
    assert_eq!(admin_queries, 4, "fixture setup, job map, and before/after invocation reads are separate");
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    let before = &before[0];
    let after = &after[0];
    let id = before.unsigned_text("job_id").unwrap().unwrap();
    assert!(id.parse::<u32>().is_ok_and(|value| value > 0));
    assert_eq!(after.unsigned_text("job_id").unwrap().as_deref(), Some(id.as_str()));
    for point in [before, after] {
        assert_eq!(point.value("is_called"), Some(Some("t")), "registration genuinely invokes the checker");
        assert_eq!(point.unsigned_text("invocations").unwrap().as_deref(), Some("1"));
    }
    assert_eq!(captured.manifest.source_queries, 8, "one source setup plus seven bounded acquisition calls");
    assert_eq!(request().budget.max_queries, 8);
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    let snapshot = &captured.snapshot;
    assert_eq!(
        snapshot.metadata.extension.as_ref().unwrap().qualifiers.get("version").map(String::as_str),
        Some("2.31.0")
    );
    assert_eq!(snapshot.metadata.view(TimescaleJobViewKind::Jobs).unwrap().state, TimescaleJobViewState::Available);
    assert_eq!(coverage(snapshot, TIMESCALE_JOBS_SOURCE).state, SourceState::Collected);
    let point = state(snapshot, &id);
    assert!(!point.scheduled);
    assert_eq!(
        point.configuration_check,
        Some(TimescaleJobProcedure {
            schema: "culprit_jobs_test".into(),
            name: "validate_native_configuration".into(),
        })
    );
    for id in fixture.jobs.values() {
        assert!(state(snapshot, id).configuration_check.is_none(), "ordinary native jobs have no invented checker");
    }
    assert!(point.observed_at.ends_with('Z'));
    assert!(snapshot.context.observed_from.ends_with('Z') && snapshot.context.observed_until.ends_with('Z'));
    assert_runs_are_partial(snapshot);
    let json = serde_json::to_string(&captured.manifest).unwrap() + &serde_json::to_string(snapshot).unwrap();
    assert!(!json.contains(PRIVATE_CONFIG));
    assert!(!json.contains(PRIVATE_ERROR));
    assert_settled(captured.manifest.read_metrics.as_ref().unwrap(), captured.manifest.source_queries);
    let recipe = crate::telemetry_recipes().into_iter().find(|recipe| recipe.id == "postgres.timescale-jobs").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
}
