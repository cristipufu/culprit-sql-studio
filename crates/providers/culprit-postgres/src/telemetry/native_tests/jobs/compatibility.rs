use super::support::*;
use culprit_core::telemetry::*;

#[tokio::test]
#[ignore = "requires nonce-owned Timescale 2.31 fixture prepared by dev/timescale/jobs.sql; see dev/timescale/jobs.md"]
async fn installed_public_job_views_retain_paused_states_actual_failures_and_exact_native_counts() {
    let fixture = Fixture::open().await;
    let captured = capture(&fixture.admin, request()).await;
    assert_eq!(captured.manifest.source_queries, 7, "setup, two metadata reads, two discoveries, states and history");
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
    assert_eq!(captured.manifest.source_queries, 7);
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
