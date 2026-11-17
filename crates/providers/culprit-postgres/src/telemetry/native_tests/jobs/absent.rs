use super::support::*;
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
#[ignore = "requires a separately nonce-marked owned PostgreSQL fixture WITHOUT Timescale; see dev/timescale/jobs.md"]
async fn absent_extension_retains_not_applicable_coverage_without_reading_job_views() {
    let specification = owned_spec("culprit_smoke").await;
    let captured = capture(&specification, request()).await;
    assert_eq!(captured.manifest.source_queries, 5, "setup, two metadata reads and two discoveries; no job view query");
    let read = captured.snapshot;
    assert!(read.metadata.extension.is_none());
    assert!(read.metadata.views.iter().all(|view| view.state == TimescaleJobViewState::Missing));
    assert!(read.metadata.run_view.is_none());
    assert!(read.metadata.successful_runs_logged.is_none());
    assert_eq!(read.metadata.run_visibility, TimescaleJobRunVisibility::Unknown);
    assert!(read.observations.is_empty());
    assert!(read.sources.iter().all(|scope| scope.state == SourceState::NotApplicable && scope.records == 0));
}
